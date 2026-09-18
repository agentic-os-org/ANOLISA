use super::*;

#[test]
fn terminal_write_releases_capacity_and_preserves_complete_deployment_record() {
    for phase in [
        BindingStatus::PendingApply,
        BindingStatus::Applying,
        BindingStatus::PendingDelete,
        BindingStatus::Deleting,
    ] {
        for code in ["RECONCILE_RETRY_EXHAUSTED", "RECONCILE_WORKER_PANICKED"] {
            let mut original = record(1);
            original.binding.status.phase = phase;
            original
                .deployments
                .push(asc_policy_repository::Deployment {
                    target: TargetRef {
                        route: "test".into(),
                        id: "uncertain-target".into(),
                        cleanup: vec![1, 2],
                    },
                    revision: original.binding.spec.binding_revision,
                    presence: Presence::Unknown,
                    last_confirmed: None,
                });
            let repo =
                ProcessLocalPapRepository::with_binding_states(vec![original.clone()]).unwrap();
            let queue = WorkQueue::new(1, 0);
            queue.enqueue(&id(1)).unwrap();
            assert_eq!(queue.take(), Some(id(1)));
            assert!(queue.finish(id(1), Some(1), true));
            assert!(matches!(
                queue.state.lock().unwrap().entries.get(&id(1)),
                Some(Entry::Running { .. })
            ));
            finish_failed(&repo, &queue, id(1), Some(&original), code);
            let mut expected = original;
            expected.binding.status.phase = if matches!(
                phase,
                BindingStatus::PendingDelete | BindingStatus::Deleting
            ) {
                BindingStatus::DeleteFailed
            } else {
                BindingStatus::ApplyFailed
            };
            expected.binding.status.error = Some(Failure::new(FailureKind::Rejected, code));
            assert_eq!(repo.get_binding_state(&id(1)).unwrap(), Some(expected));
            assert!(queue.state.lock().unwrap().entries.is_empty());
            assert!(queue.state.lock().unwrap().schedules.is_empty());
            queue.enqueue(&id(2)).unwrap();
        }
    }
}

struct FaultedTerminalWrite {
    inner: ProcessLocalPapRepository,
    mode: u8,
}
impl BindingStateRepository for FaultedTerminalWrite {
    fn get_binding_state(
        &self,
        id: &ResourceId,
    ) -> Result<Option<BindingStateSnapshot>, StoreError> {
        self.inner.get_binding_state(id)
    }
    fn compare_exchange_binding_state(
        &self,
        expected: &BindingStateSnapshot,
        write: &BindingStateWrite,
    ) -> Result<WriteResult, StoreError> {
        match self.mode {
            0 => Err(StoreError::Unavailable),
            1 => panic!("terminal write panicked"),
            _ => {
                let mut next = expected.binding.clone();
                next.status = BindingStatus::PendingDelete.into();
                self.inner
                    .update_binding(Some(&expected.binding), &next)
                    .unwrap();
                self.inner.compare_exchange_binding_state(expected, write)
            }
        }
    }
}

#[test]
fn unconfirmed_write_retains_slot_and_conflict_preserves_new_delete() {
    for mode in 0..=2 {
        let original = record(1);
        let repo = FaultedTerminalWrite {
            inner: ProcessLocalPapRepository::with_binding_states(vec![original.clone()]).unwrap(),
            mode,
        };
        let queue = WorkQueue::new(1, 0);
        queue.enqueue(&id(1)).unwrap();
        assert_eq!(queue.take(), Some(id(1)));
        finish_failed(
            &repo,
            &queue,
            id(1),
            Some(&original),
            "RECONCILE_RETRY_EXHAUSTED",
        );
        let saved = repo.get_binding_state(&id(1)).unwrap().unwrap();
        if mode == 2 {
            assert_eq!(saved.binding.status.phase, BindingStatus::PendingDelete);
            assert_eq!(saved.binding.status.error, None);
            assert_eq!(queue.take(), Some(id(1)));
        } else {
            assert_eq!(saved, original);
            assert_eq!(
                queue.state.lock().unwrap().entries.get(&id(1)),
                Some(&Entry::Exhausted)
            );
            queue.discover_many([(id(1), None)], 100);
            assert!(queue.state.lock().unwrap().ready.is_empty());
        }
        assert!(queue.is_healthy());
    }
}

#[test]
fn newer_intent_or_notification_is_not_failed_by_old_attempt() {
    for mode in 0..3 {
        let original = record(1);
        let mut current = original.clone();
        if mode == 0 {
            current.binding.spec.binding_revision = asc_foundation_types::Revision::new(8).unwrap();
        }
        if mode == 1 {
            current.binding.status.phase = BindingStatus::PendingDelete;
        }
        let repo = ProcessLocalPapRepository::with_binding_states(vec![current.clone()]).unwrap();
        let queue = WorkQueue::new(1, 0);
        queue.enqueue(&id(1)).unwrap();
        assert_eq!(queue.take(), Some(id(1)));
        if mode == 2 {
            queue.enqueue(&id(1)).unwrap();
        }
        finish_failed(
            &repo,
            &queue,
            id(1),
            Some(&original),
            "RECONCILE_WORKER_PANICKED",
        );
        assert_eq!(repo.get_binding_state(&id(1)).unwrap(), Some(current));
        assert_eq!(queue.take(), Some(id(1)));
    }
}

struct PanickingSkippedRead {
    inner: Arc<ProcessLocalPapRepository>,
    reads: AtomicUsize,
}
impl BindingStateRepository for PanickingSkippedRead {
    fn get_binding_state(
        &self,
        id_value: &ResourceId,
    ) -> Result<Option<BindingStateSnapshot>, StoreError> {
        assert!(
            !(id_value == &id(1) && self.reads.fetch_add(1, Ordering::SeqCst) == 1),
            "Skipped scheduling read panicked"
        );
        self.inner.get_binding_state(id_value)
    }
    fn compare_exchange_binding_state(
        &self,
        expected: &BindingStateSnapshot,
        write: &BindingStateWrite,
    ) -> Result<WriteResult, StoreError> {
        self.inner.compare_exchange_binding_state(expected, write)
    }
}
impl BindingReconcileCatalog for PanickingSkippedRead {
    fn scan_reconciliation(
        &self,
        after: Option<&ResourceId>,
        limit: usize,
    ) -> Result<Vec<asc_policy_repository::ReconcileCandidate>, StoreError> {
        self.inner.scan_reconciliation(after, limit)
    }
}

#[test]
fn skipped_read_panic_is_terminalized_without_stopping_other_bindings() {
    let inner = Arc::new(
        ProcessLocalPapRepository::with_binding_states(vec![record(1), record(2)]).unwrap(),
    );
    let repo = Arc::new(PanickingSkippedRead {
        inner: inner.clone(),
        reads: AtomicUsize::new(0),
    });
    let attempt = Arc::new(RepeatedOutcome {
        core: core(
            repo.clone(),
            Arc::new(Client::default()),
            Arc::new(TestClock::default()),
        ),
        result: Ok(Disposition::Skipped),
        calls: Mutex::new(vec![]),
    });
    let runtime = ReconciliationRuntime::start(
        repo,
        attempt,
        RuntimeConfig {
            workers: 1,
            tick_interval: Duration::from_millis(1),
            ..RuntimeConfig::default()
        },
    )
    .unwrap();
    let queue = runtime.enqueuer();
    wait_until(|| {
        status(&inner, 1) == Some(BindingStatus::ApplyFailed)
            && status(&inner, 2) == Some(BindingStatus::Ready)
            && queue.state.lock().unwrap().entries.is_empty()
    });
    assert_eq!(
        inner
            .get_binding_state(&id(1))
            .unwrap()
            .unwrap()
            .binding
            .status
            .error,
        Some(Failure::new(
            FailureKind::Rejected,
            "RECONCILE_WORKER_PANICKED"
        ))
    );
    assert!(queue.is_healthy());
    runtime.shutdown().unwrap();
}

struct GatedTerminalWrite {
    inner: Arc<ProcessLocalPapRepository>,
    gate: Mutex<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>>,
}
impl BindingStateRepository for GatedTerminalWrite {
    fn get_binding_state(
        &self,
        id: &ResourceId,
    ) -> Result<Option<BindingStateSnapshot>, StoreError> {
        self.inner.get_binding_state(id)
    }
    fn compare_exchange_binding_state(
        &self,
        expected: &BindingStateSnapshot,
        write: &BindingStateWrite,
    ) -> Result<WriteResult, StoreError> {
        let (entered, release) = self.gate.lock().unwrap().take().unwrap();
        entered.send(()).unwrap();
        release.recv_timeout(Duration::from_secs(5)).unwrap();
        self.inner.compare_exchange_binding_state(expected, write)
    }
}

#[test]
fn failed_write_keeps_running_ownership_while_new_delete_wins_cas() {
    let original = record(1);
    let inner =
        Arc::new(ProcessLocalPapRepository::with_binding_states(vec![original.clone()]).unwrap());
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let repo = Arc::new(GatedTerminalWrite {
        inner: inner.clone(),
        gate: Mutex::new(Some((entered_tx, release_rx))),
    });
    let queue = Arc::new(WorkQueue::new(1, 0));
    queue.enqueue(&id(1)).unwrap();
    assert_eq!(queue.take(), Some(id(1)));
    assert!(queue.finish(id(1), Some(1), true));
    let task = {
        let queue = queue.clone();
        std::thread::spawn(move || {
            finish_failed(
                repo.as_ref(),
                &queue,
                id(1),
                Some(&original),
                "RECONCILE_RETRY_EXHAUSTED",
            );
        })
    };
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(queue.enqueue(&id(2)), Err(asc_pap::EnqueueError::Full));
    let pap = PapService::new(
        inner.clone(),
        Arc::new(asc_policy_engine::PolicyTemplateCompiler),
    )
    .with_reconcile_enqueuer(queue.clone());
    let accepted = pap.delete_binding(&id(1)).unwrap();
    assert!(queue.state.lock().unwrap().ready.is_empty());
    assert!(matches!(
        queue.state.lock().unwrap().entries.get(&id(1)),
        Some(Entry::Running { dirty: true, .. })
    ));
    release_tx.send(()).unwrap();
    task.join().unwrap();
    assert_eq!(inner.get_binding(&id(1)).unwrap(), accepted);
    assert_eq!(accepted.status.phase, BindingStatus::PendingDelete);
    assert_eq!(queue.take(), Some(id(1)));
}
