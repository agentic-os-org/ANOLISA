//! Attempt panic isolation with real core bookkeeping and scripted port faults.
use super::*;
use asc_policy_repository::{BindingStateWrite, Deployment, ReconcileCandidate, WriteResult};

fn registered(client: &Client, failed: bool) -> BindingStateSnapshot {
    let mut expected = record(1);
    expected.binding.status.phase = if failed {
        BindingStatus::ApplyFailed
    } else {
        BindingStatus::Applying
    };
    expected.binding.status.error =
        failed.then(|| Failure::new(FailureKind::Rejected, "RECONCILE_WORKER_PANICKED"));
    expected.deployments = vec![Deployment {
        target: client.requests.lock().unwrap()[0].target.clone(),
        revision: expected.binding.spec.binding_revision,
        presence: Presence::Unknown,
        last_confirmed: None,
    }];
    expected
}

#[test]
fn attempt_panic_records_failure_and_same_worker_completes_next_binding() {
    let repo = Arc::new(
        ProcessLocalPapRepository::with_binding_states(vec![record(1), record(2)]).unwrap(),
    );
    let client = Arc::new(Client::default());
    client.panic_on_create.store(true, Ordering::SeqCst);
    let service = start(
        repo.clone(),
        client.clone(),
        Arc::new(TestClock::default()),
        2,
    );
    let queue = service.enqueuer();
    wait_until(|| {
        status(&repo, 2) == Some(BindingStatus::Ready)
            && queue.state.lock().unwrap().entries.is_empty()
    });
    assert_eq!(
        repo.get_binding_state(&id(1)).unwrap(),
        Some(registered(&client, true))
    );
    assert_eq!(
        *client.calls.lock().unwrap(),
        vec![format!("apply:{}", id(1)), format!("apply:{}", id(2))]
    );
    assert!(queue.is_healthy());
    assert!(!queue.has_failed());
    assert_eq!(queue.check_ready(), Ok(()));
    exercise_independent_pap_crud(&repo, &queue);
    service.shutdown().unwrap();
}

#[test]
fn delete_during_attempt_panic_keeps_dirty_and_cleans_registered_target() {
    let repo = Arc::new(ProcessLocalPapRepository::with_binding_states(vec![record(1)]).unwrap());
    let client = Arc::new(Client::default());
    client.panic_on_create.store(true, Ordering::SeqCst);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    *client.gate.lock().unwrap() = Some((entered_tx, release_rx));
    let service = start(
        repo.clone(),
        client.clone(),
        Arc::new(TestClock::default()),
        1,
    );
    let queue = service.enqueuer();
    let pap = PapService::new(
        repo.clone(),
        Arc::new(asc_policy_engine::PolicyTemplateCompiler),
    )
    .with_reconcile_enqueuer(queue.clone());
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let accepted = pap.delete_binding(&id(1)).unwrap();
    let mut expected = registered(&client, false);
    expected.binding = accepted;

    assert_eq!(repo.get_binding_state(&id(1)).unwrap(), Some(expected));
    assert_eq!(
        queue.state.lock().unwrap().entries.get(&id(1)),
        Some(&Entry::Running {
            dirty: true,
            retries: 0
        })
    );
    release_tx.send(()).unwrap();
    wait_until(|| status(&repo, 1).is_none() && queue.state.lock().unwrap().entries.is_empty());
    assert_eq!(
        *client.calls.lock().unwrap(),
        vec![format!("apply:{}", id(1)), format!("delete:{}", id(1))]
    );
    assert!(queue.is_healthy());
    service.shutdown().unwrap();
}

struct PanicAfterCompletion {
    core: Arc<BindingReconciler>,
    calls: Mutex<Vec<ResourceId>>,
}
impl ReconcileAttempt for PanicAfterCompletion {
    fn clock(&self) -> Arc<dyn Clock> {
        self.core.clock()
    }
    fn reconcile(
        &self,
        binding_id: &ResourceId,
        schedule: &mut AttemptSchedule,
    ) -> Result<Disposition, StoreError> {
        self.calls.lock().unwrap().push(binding_id.clone());
        let result = self.core.reconcile(binding_id, schedule);
        assert_ne!(binding_id, &id(1), "scripted post-completion panic");
        result
    }
}

#[test]
fn committed_success_survives_attempt_panic_without_replay() {
    for deleting in [false, true] {
        let mut initial = record(1);
        if deleting {
            initial.binding.status.phase = BindingStatus::PendingDelete;
        }
        let repo = Arc::new(
            ProcessLocalPapRepository::with_binding_states(vec![initial, record(2)]).unwrap(),
        );
        let clock = Arc::new(TestClock::default());
        let client = Arc::new(Client::default());
        let attempt = Arc::new(PanicAfterCompletion {
            core: core(repo.clone(), client.clone(), clock.clone()),
            calls: Mutex::new(vec![]),
        });
        let service = ReconciliationRuntime::start(
            repo.clone(),
            attempt.clone(),
            RuntimeConfig {
                workers: 1,
                tick_interval: Duration::from_millis(1),
                ..RuntimeConfig::default()
            },
        )
        .unwrap();
        let queue = service.enqueuer();
        wait_until(|| {
            status(&repo, 2) == Some(BindingStatus::Ready)
                && queue.state.lock().unwrap().entries.is_empty()
        });
        let expected = if deleting {
            None
        } else {
            let mut saved = registered(&client, false);
            saved.binding.status.phase = BindingStatus::Ready;
            saved.deployments[0].presence = Presence::Present;
            saved.deployments[0].last_confirmed = Some(Presence::Present);
            Some(saved)
        };
        assert_eq!(repo.get_binding_state(&id(1)).unwrap(), expected);
        assert_eq!(*attempt.calls.lock().unwrap(), vec![id(1), id(2)]);
        let expected_calls = if deleting {
            vec![format!("apply:{}", id(2))]
        } else {
            vec![format!("apply:{}", id(1)), format!("apply:{}", id(2))]
        };
        assert_eq!(*client.calls.lock().unwrap(), expected_calls);
        assert!(queue.is_healthy());
        service.shutdown().unwrap();
    }
}

struct FailedBookkeeping {
    inner: Arc<ProcessLocalPapRepository>,
    failed: AtomicBool,
    read_fault: u8,
}
impl BindingStateRepository for FailedBookkeeping {
    fn get_binding_state(
        &self,
        binding_id: &ResourceId,
    ) -> Result<Option<BindingStateSnapshot>, StoreError> {
        if binding_id == &id(1) && self.failed.load(Ordering::SeqCst) {
            match self.read_fault {
                1 => return Err(StoreError::Unavailable),
                2 => panic!("scripted panic during outcome verification"),
                _ => {}
            }
        }
        self.inner.get_binding_state(binding_id)
    }
    fn compare_exchange_binding_state(
        &self,
        expected: &BindingStateSnapshot,
        write: &BindingStateWrite,
    ) -> Result<WriteResult, StoreError> {
        if expected.binding.spec.binding_id == id(1)
            && write.next.as_ref().is_some_and(|patch| {
                patch
                    .status
                    .as_ref()
                    .is_some_and(|s| s.phase == BindingStatus::ApplyFailed)
            })
        {
            self.failed.store(true, Ordering::SeqCst);
            return Err(StoreError::Unavailable);
        }
        self.inner.compare_exchange_binding_state(expected, write)
    }
}
impl BindingReconcileCatalog for FailedBookkeeping {
    fn scan_reconciliation(
        &self,
        after: Option<&ResourceId>,
        limit: usize,
    ) -> Result<Vec<ReconcileCandidate>, StoreError> {
        self.inner.scan_reconciliation(after, limit)
    }
}

#[test]
fn unconfirmed_panic_stops_only_that_binding_until_new_notification() {
    // Verify readable Running state, read error, and a second port panic.
    for read_fault in 0..=2 {
        let inner = Arc::new(
            ProcessLocalPapRepository::with_binding_states(vec![record(1), record(2)]).unwrap(),
        );
        let repo = Arc::new(FailedBookkeeping {
            inner: inner.clone(),
            failed: AtomicBool::new(false),
            read_fault,
        });
        let clock = Arc::new(TestClock::default());
        let client = Arc::new(Client::default());
        client.panic_on_create.store(true, Ordering::SeqCst);
        let service = ReconciliationRuntime::start(
            repo.clone(),
            core(repo.clone(), client.clone(), clock.clone()),
            RuntimeConfig {
                workers: 1,
                tick_interval: Duration::from_millis(1),
                ..RuntimeConfig::default()
            },
        )
        .unwrap();
        let queue = service.enqueuer();
        wait_until(|| {
            status(&inner, 2) == Some(BindingStatus::Ready)
                && queue.state.lock().unwrap().entries.get(&id(1)) == Some(&Entry::Exhausted)
        });
        assert_eq!(
            inner.get_binding_state(&id(1)).unwrap(),
            Some(registered(&client, false))
        );
        queue.tick(u64::MAX);
        queue.discover_many([(id(1), None)], u64::MAX);
        assert!(queue.state.lock().unwrap().ready.is_empty());
        assert_eq!(
            *client.calls.lock().unwrap(),
            vec![format!("apply:{}", id(1)), format!("apply:{}", id(2))]
        );
        exercise_independent_pap_crud(&inner, &queue);
        assert!(queue.is_healthy());
        assert_eq!(queue.check_ready(), Ok(()));
        repo.failed.store(false, Ordering::SeqCst);
        let pap = PapService::new(
            inner.clone(),
            Arc::new(asc_policy_engine::PolicyTemplateCompiler),
        )
        .with_reconcile_enqueuer(queue.clone());
        pap.delete_binding(&id(1)).unwrap();
        wait_until(|| {
            status(&inner, 1).is_none() && queue.state.lock().unwrap().entries.is_empty()
        });
        assert_eq!(
            client.calls.lock().unwrap().last(),
            Some(&format!("delete:{}", id(1)))
        );
        service.shutdown().unwrap();
    }
}

#[test]
fn panic_completion_and_new_notification_race_never_loses_work() {
    for terminal in [false, true] {
        for _ in 0..32 {
            let queue = Arc::new(WorkQueue::new(1, 4));
            let _ = queue.enqueue(&id(1));
            assert_eq!(queue.take(), Some(id(1)));
            let barrier = Arc::new(Barrier::new(2));
            let other_queue = queue.clone();
            let other_barrier = barrier.clone();
            let notifier = thread::spawn(move || {
                other_barrier.wait();
                let _ = other_queue.enqueue(&id(1));
            });
            barrier.wait();
            queue.finish_terminalization(id(1), terminal);
            notifier.join().unwrap();
            let state = queue.state.lock().unwrap();
            assert_eq!(
                state.entries,
                BTreeMap::from([(id(1), Entry::Queued { retries: 0 })])
            );
            assert_eq!(state.ready, VecDeque::from([id(1)]));
        }
    }
}
