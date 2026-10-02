//! Journal acknowledgement gates calls and terminal results.

use super::*;

#[test]
fn journal_claim_or_pre_dispatch_append_failure_prevents_call() {
    for (fail_claim, fail_append) in [(true, None), (false, Some(0)), (false, Some(1))] {
        let core = Core::new().unwrap();
        let mut host = Host::new();
        let mut journal = MemoryJournal {
            fail_claim,
            fail_append,
            ..MemoryJournal::default()
        };
        let prepared = core.prepare(request(false), &host, 1000).unwrap();
        assert!(matches!(
            core.execute(
                prepared,
                &mut host,
                &mut journal,
                &FixedClock(1100),
                &NeverCancel
            ),
            Err(Error::Journal(_))
        ));
        assert!(host.invoked.is_empty());
        assert!(journal.writers.is_empty());
        assert_eq!(journal.releases, usize::from(!fail_claim));
    }
}

#[test]
fn settlement_write_failure_stops_later_calls_and_keeps_claim() {
    let core = Core::new().unwrap();
    let mut host = Host::new();
    let mut journal = MemoryJournal {
        fail_append: Some(2),
        ..MemoryJournal::default()
    };
    let prepared = core.prepare(request(false), &host, 1000).unwrap();
    assert!(matches!(
        core.execute(
            prepared,
            &mut host,
            &mut journal,
            &FixedClock(1100),
            &NeverCancel
        ),
        Err(Error::Journal(_))
    ));
    assert_eq!(host.invoked.len(), 1);
    assert_eq!(journal.claims.len(), 1);
    assert!(journal.writers.is_empty());
    assert_eq!(journal.releases, 1);
    let retry = core.prepare(request(false), &host, 1000).unwrap();
    assert!(matches!(
        core.execute(
            retry,
            &mut host,
            &mut journal,
            &FixedClock(1100),
            &NeverCancel
        ),
        Err(Error::Journal(JournalError::AlreadyClaimed))
    ));
    assert_eq!(host.invoked.len(), 1);
}

#[test]
fn completed_event_cannot_be_reexecuted_with_a_changed_plan() {
    let core = Core::new().unwrap();
    let mut host = Host::new();
    let mut journal = MemoryJournal::default();
    let first = core.prepare(request(false), &host, 1000).unwrap();
    core.execute(
        first,
        &mut host,
        &mut journal,
        &FixedClock(1100),
        &NeverCancel,
    )
    .unwrap();
    let mut req = request(false);
    req.plan["revision"] = json!(2);
    let second = core.prepare(req, &host, 1000).unwrap();
    assert!(matches!(
        core.execute(
            second,
            &mut host,
            &mut journal,
            &FixedClock(1100),
            &NeverCancel
        ),
        Err(Error::Journal(JournalError::AlreadyClaimed))
    ));
    assert_eq!(host.invoked.len(), 2);
}

#[test]
fn terminal_journal_failure_never_returns_a_usable_execution() {
    let core = Core::new().unwrap();
    let mut host = Host::new();
    let mut journal = MemoryJournal {
        fail_append: Some(8),
        ..MemoryJournal::default()
    };
    let prepared = core.prepare(request(false), &host, 1000).unwrap();
    assert!(matches!(
        core.execute(
            prepared,
            &mut host,
            &mut journal,
            &FixedClock(1100),
            &NeverCancel
        ),
        Err(Error::Journal(_))
    ));
    assert_eq!(host.invoked.len(), 2);
    assert_eq!(journal.records.last().unwrap()["kind"], "step_settled");
    assert_eq!(journal.claims.len(), 1);
    assert!(journal.writers.is_empty());
    assert_eq!(journal.releases, 1);
}

struct MalformedJournal {
    inner: MemoryJournal,
    at: usize,
    acknowledgement: Value,
}

impl Journal for MalformedJournal {
    fn claim(&mut self, event_key: &str, plan: &Value) -> Result<Value, JournalError> {
        let valid = self.inner.claim(event_key, plan)?;
        Ok(if self.at == 0 {
            self.acknowledgement.clone()
        } else {
            valid
        })
    }

    fn append(&mut self, event_key: &str, record: &Value) -> Result<Value, JournalError> {
        let valid = self.inner.append(event_key, record)?;
        Ok(if self.at == self.inner.records.len() {
            self.acknowledgement.clone()
        } else {
            valid
        })
    }

    fn release(&mut self, event_key: &str) {
        self.inner.release(event_key);
    }
}

#[test]
fn malformed_acknowledgements_stop_at_every_storage_boundary() {
    let core = Core::new().unwrap();
    let valid = ack(&json!({}), "record");
    let mut missing_digest = valid.clone();
    missing_digest.as_object_mut().unwrap().remove("digest");
    let mut invalid_digest = valid.clone();
    invalid_digest["digest"] = json!("g".repeat(64));
    let mut newline = valid.clone();
    newline["source_id"] = json!("journal\n");
    let mut extra = valid;
    extra["extra"] = json!(true);
    for invalid in [Value::Null, missing_digest, invalid_digest, newline, extra] {
        // Claim, then each step's start/call-start/call-settlement/settlement,
        // then terminal acknowledgement. No subsequent effect may pass a bad ack.
        for (at, expected_calls) in [0, 0, 0, 1, 1, 1, 1, 2, 2, 2].into_iter().enumerate() {
            let mut host = Host::new();
            let mut journal = MalformedJournal {
                inner: MemoryJournal::default(),
                at,
                acknowledgement: invalid.clone(),
            };
            let prepared = core.prepare(request(false), &host, 1000).unwrap();
            assert!(matches!(
                core.execute(
                    prepared,
                    &mut host,
                    &mut journal,
                    &FixedClock(1100),
                    &NeverCancel,
                ),
                Err(Error::Contract(_))
            ));
            assert_eq!(host.invoked.len(), expected_calls, "acknowledgement {at}");
            assert_eq!(journal.inner.records.len(), at);
            assert_eq!(journal.inner.claims.len(), 1);
            assert!(journal.inner.writers.is_empty());
            assert_eq!(journal.inner.releases, 1);
            let retry = core.prepare(request(false), &host, 1000).unwrap();
            assert!(matches!(
                core.execute(
                    retry,
                    &mut host,
                    &mut journal,
                    &FixedClock(1100),
                    &NeverCancel,
                ),
                Err(Error::Journal(JournalError::AlreadyClaimed))
            ));
            assert_eq!(host.invoked.len(), expected_calls);
            assert_eq!(journal.inner.releases, 1);
        }
    }
}
