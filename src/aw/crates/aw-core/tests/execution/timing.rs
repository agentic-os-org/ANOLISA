//! Dispatch admission and observed time budgets.

use super::*;

#[test]
fn expired_dispatch_deadline_prevents_host_call() {
    let core = Core::new().unwrap();
    let mut host = Host::new();
    let prepared = core.prepare(request(false), &host, 1000).unwrap();
    assert!(core
        .execute(
            prepared,
            &mut host,
            &mut MemoryJournal::default(),
            &FixedClock(2001),
            &NeverCancel
        )
        .is_err());
    assert!(host.invoked.is_empty());
}

#[test]
fn receipt_cannot_claim_time_outside_observed_call() {
    let core = Core::new().unwrap();
    let mut host = Host::new();
    host.replies = vec![Reply::LateReceipt];
    let prepared = core.prepare(request(false), &host, 1000).unwrap();
    assert!(matches!(
        core.execute(
            prepared,
            &mut host,
            &mut MemoryJournal::default(),
            &FixedClock(1100),
            &NeverCancel
        ),
        Err(Error::HostTime)
    ));
    assert_eq!(host.invoked.len(), 1);
}

struct SharedClock(Rc<Cell<u64>>);
impl Clock for SharedClock {
    fn now_ms(&self) -> u64 {
        self.0.get()
    }
}

#[derive(Default)]
struct InterleavingJournal {
    inner: MemoryJournal,
    advance_on_start: Option<(Rc<Cell<u64>>, u64)>,
    cancel_on_start: Option<Rc<Cell<bool>>>,
}
impl Journal for InterleavingJournal {
    fn claim(&mut self, event_key: &str, plan: &Value) -> Result<Value, JournalError> {
        self.inner.claim(event_key, plan)
    }

    fn append(&mut self, event_key: &str, record: &Value) -> Result<Value, JournalError> {
        let evidence = self.inner.append(event_key, record)?;
        if record["kind"] == "invocation_started" {
            // Simulate an acknowledged fsync overlapping time or cancellation.
            if let Some((clock, now)) = self.advance_on_start.take() {
                clock.set(now);
            }
            if let Some(cancelled) = self.cancel_on_start.take() {
                cancelled.set(true);
            }
        }
        Ok(evidence)
    }

    fn release(&mut self, event_key: &str) {
        self.inner.release(event_key);
    }
}

struct TimedHost {
    inner: Host,
    clock: Rc<Cell<u64>>,
    call_duration: u64,
}
impl ProviderHost for TimedHost {
    fn descriptor(&self, id: &str) -> Option<&Value> {
        self.inner.descriptor(id)
    }

    fn invoke(&mut self, invocation: &Value) -> Result<ProviderResult, HostError> {
        let start = self.clock.get();
        let mut result = self.inner.invoke(invocation)?;
        self.clock.set(start + self.call_duration);
        result.receipt["started_at_ms"] = json!(start);
        result.receipt["completed_at_ms"] = json!(self.clock.get());
        Ok(result)
    }
}

#[test]
fn journal_sync_crossing_deadline_prevents_actual_dispatch() {
    let core = Core::new().unwrap();
    let mut host = Host::new();
    let clock = Rc::new(Cell::new(1100));
    let mut journal = InterleavingJournal {
        advance_on_start: Some((clock.clone(), 2001)),
        ..InterleavingJournal::default()
    };
    let prepared = core.prepare(request(false), &host, 1000).unwrap();
    assert!(matches!(
        core.execute(
            prepared,
            &mut host,
            &mut journal,
            &SharedClock(clock),
            &NeverCancel,
        ),
        Err(Error::Contract(_))
    ));
    assert!(host.invoked.is_empty());
    assert_eq!(journal.inner.claims.len(), 1);
    assert_eq!(
        journal.inner.records.last().unwrap()["kind"],
        "invocation_started"
    );
    assert!(journal
        .inner
        .records
        .iter()
        .all(|record| record["kind"] != "invocation_settled"));
}

#[test]
fn journal_sync_duration_does_not_consume_host_wall_time_budget() {
    let core = Core::new().unwrap();
    let clock = Rc::new(Cell::new(1100));
    let mut host = TimedHost {
        inner: Host::new(),
        clock: clock.clone(),
        call_duration: 5,
    };
    let mut journal = InterleavingJournal {
        advance_on_start: Some((clock.clone(), 1600)),
        ..InterleavingJournal::default()
    };
    let mut req = request(false);
    for input in req.inputs.values_mut() {
        input.budget["wall_time_ms"] = json!(50);
    }
    let prepared = core.prepare(req, &host, 1000).unwrap();
    let result = core
        .execute(
            prepared,
            &mut host,
            &mut journal,
            &SharedClock(clock.clone()),
            &NeverCancel,
        )
        .unwrap();
    assert_eq!(result.record()["decision"], "proceed");
    assert_eq!(host.inner.invoked.len(), 2);
    assert_eq!(result.calls()[0].result().receipt["started_at_ms"], 1600);
    assert_eq!(result.calls()[0].result().receipt["completed_at_ms"], 1605);
    assert_eq!(clock.get(), 1610);
}

#[test]
fn cancellation_during_start_sync_prevents_call_without_fabricating_receipt() {
    let core = Core::new().unwrap();
    let mut host = Host::new();
    let flag = Rc::new(Cell::new(false));
    let mut journal = InterleavingJournal {
        cancel_on_start: Some(flag.clone()),
        ..InterleavingJournal::default()
    };
    let prepared = core.prepare(request(false), &host, 1000).unwrap();
    let result = core
        .execute(
            prepared,
            &mut host,
            &mut journal,
            &FixedClock(1100),
            &CancelFlag(flag),
        )
        .unwrap();
    assert_eq!(result.record()["decision"], "cancelled");
    assert_eq!(result.record()["steps"][0]["outcome"], "cancelled");
    assert_eq!(result.record()["steps"][1]["outcome"], "skipped");
    assert_eq!(result.record()["steps"][0]["invocations"], json!([]));
    assert!(result.calls().is_empty());
    assert!(host.invoked.is_empty());
    assert!(journal
        .inner
        .records
        .iter()
        .any(|record| record["kind"] == "invocation_started"));
    assert!(journal
        .inner
        .records
        .iter()
        .all(|record| record["kind"] != "invocation_settled"));
}
