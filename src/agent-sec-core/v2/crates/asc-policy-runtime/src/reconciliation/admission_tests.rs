use super::*;
use asc_pap::{EnqueueError, PapError};
use asc_policy_repository::{BindingStateWrite, ReconciliationPatch, WriteResult};

fn pap_with(
    repo: Arc<ProcessLocalPapRepository>,
    queue: Arc<dyn BindingReconcileEnqueuer>,
) -> PapService<ProcessLocalPapRepository, asc_policy_engine::PolicyTemplateCompiler> {
    PapService::new(repo, Arc::new(asc_policy_engine::PolicyTemplateCompiler))
        .with_reconcile_enqueuer(queue)
}

#[test]
fn admission_full_failed_snapshot_stale_wakeup_and_explicit_retry() {
    for deleting in [false, true] {
        let mut initial = record(1);
        if deleting {
            initial.binding.status.phase = BindingStatus::Ready;
            initial.deployments.push(asc_policy_repository::Deployment {
                target: TargetRef {
                    route: "test".into(),
                    id: id(1).to_string(),
                    cleanup: vec![1],
                },
                revision: initial.binding.spec.binding_revision,
                presence: Presence::Unknown,
                last_confirmed: None,
            });
        }
        let repo = Arc::new(
            ProcessLocalPapRepository::with_binding_states(vec![initial.clone()]).unwrap(),
        );
        let queue = Arc::new(WorkQueue::new(1, 4));
        queue.enqueue(&id(2)).unwrap();
        let pap = pap_with(repo.clone(), queue.clone());
        let spec = &initial.binding.spec;
        let request = || {
            if deleting {
                pap.delete_binding(&id(1))
            } else {
                pap.update_binding(
                    &id(1),
                    &spec.policy.policy_id,
                    spec.policy.revision,
                    &spec.scope.scope_id,
                    spec.scope.revision,
                )
            }
        };
        let returned = request().unwrap();
        let mut expected = initial.clone();
        expected.binding.status.phase = if deleting {
            BindingStatus::DeleteFailed
        } else {
            BindingStatus::ApplyFailed
        };
        expected.binding.status.error = Some(EnqueueError::Full.failure());
        assert_eq!(returned, expected.binding);
        assert_eq!(
            repo.get_binding_state(&id(1)).unwrap(),
            Some(expected.clone())
        );
        let mut view = expected.binding.clone();
        view.status.error = expected.binding.status.error.clone();
        assert_eq!(pap.get_binding(&id(1)).unwrap(), view);
        assert_eq!(pap.list_bindings(10, 0).unwrap().items, vec![view]);

        // A scanner may have collected this ID before the failure transaction.
        assert_eq!(queue.take(), Some(id(2)));
        queue.finish(id(2), None, false);
        queue.discover_many(vec![(id(1), None)], 0);
        assert_eq!(queue.take(), Some(id(1)));
        let client = Arc::new(Client::default());
        let reconciler = core(repo.clone(), client.clone(), Arc::new(TestClock::default()));
        reconciler
            .reconcile(&id(1), &mut asc_pcp::AttemptSchedule::default())
            .unwrap();
        assert!(client.calls.lock().unwrap().is_empty());
        assert_eq!(client.preparations.load(Ordering::SeqCst), 0);
        assert_eq!(repo.get_binding_state(&id(1)).unwrap(), Some(expected));
        queue.finish(id(1), None, false);

        let retried = request().unwrap();
        assert_eq!(retried.spec, *spec);
        assert_eq!(retried.status.error, None);
        assert_eq!(repo.get_binding(&id(1)).unwrap().status.error, None);
        assert_eq!(queue.take(), Some(id(1)));
        reconciler
            .reconcile(&id(1), &mut asc_pcp::AttemptSchedule::default())
            .unwrap();
        assert_eq!(
            *client.calls.lock().unwrap(),
            if deleting {
                vec![format!("delete:{}", id(1))]
            } else {
                vec![format!("apply:{}", id(1))]
            }
        );
    }
}

struct InterleavingEnqueuer {
    repo: Arc<ProcessLocalPapRepository>,
    claim: bool,
}
impl BindingReconcileEnqueuer for InterleavingEnqueuer {
    fn check_ready(&self) -> Result<(), PapError> {
        Ok(())
    }
    fn enqueue(&self, id: &ResourceId) -> Result<(), EnqueueError> {
        if self.claim {
            let current = self.repo.get_binding_state(id).unwrap().unwrap();
            assert_eq!(
                self.repo
                    .compare_exchange_binding_state(
                        &current,
                        &BindingStateWrite::patch(ReconciliationPatch {
                            status: Some(
                                current
                                    .binding
                                    .status
                                    .phase
                                    .start_reconcile()
                                    .unwrap()
                                    .into()
                            ),
                            ..ReconciliationPatch::default()
                        })
                    )
                    .unwrap(),
                WriteResult::Applied
            );
        }
        Err(EnqueueError::Stopped)
    }
}

#[test]
fn admission_worker_claim_wins_or_stopped_failure_is_recorded() {
    for claim in [false, true] {
        for deleting in [false, true] {
            let initial = record(1);
            let repo = Arc::new(
                ProcessLocalPapRepository::with_binding_states(vec![initial.clone()]).unwrap(),
            );
            let pap = pap_with(
                repo.clone(),
                Arc::new(InterleavingEnqueuer {
                    repo: repo.clone(),
                    claim,
                }),
            );
            let spec = &initial.binding.spec;
            let result = if deleting {
                pap.delete_binding(&id(1))
            } else {
                pap.update_binding(
                    &id(1),
                    &spec.policy.policy_id,
                    spec.policy.revision,
                    &spec.scope.scope_id,
                    spec.scope.revision,
                )
            };
            if claim {
                let current = result.unwrap();
                assert_eq!(
                    current.status,
                    if deleting {
                        BindingStatus::Deleting
                    } else {
                        BindingStatus::Applying
                    }
                );
                assert_eq!(current.status.error, None);
            } else {
                let returned = result.unwrap();
                assert_eq!(returned, pap.get_binding(&id(1)).unwrap());
                assert_eq!(
                    pap.get_binding(&id(1)).unwrap().status.error,
                    Some(EnqueueError::Stopped.failure())
                );
            }
        }
    }
}

#[test]
fn admission_pending_cas_preserves_state_and_fences_revision_and_status() {
    let initial = record(1);
    let repo = ProcessLocalPapRepository::with_binding_states(vec![initial.clone()]).unwrap();
    let mut stale = initial.binding.clone();
    stale.spec.binding_revision = stale.spec.binding_revision.checked_next().unwrap();
    assert!(
        !repo
            .fail_pending_binding(&stale, EnqueueError::Full)
            .unwrap()
    );
    stale = initial.binding.clone();
    stale.status = BindingStatus::PendingDelete.into();
    assert!(
        !repo
            .fail_pending_binding(&stale, EnqueueError::Full)
            .unwrap()
    );
    assert_eq!(
        repo.get_binding_state(&id(1)).unwrap(),
        Some(initial.clone())
    );
    assert!(
        repo.fail_pending_binding(&initial.binding, EnqueueError::Full)
            .unwrap()
    );
    let mut expected = initial.clone();
    expected.binding.status.phase = BindingStatus::ApplyFailed;
    expected.binding.status.error = Some(EnqueueError::Full.failure());
    assert_eq!(repo.get_binding_state(&id(1)).unwrap(), Some(expected));
    assert_eq!(
        repo.compare_exchange_binding_state(
            &initial,
            &BindingStateWrite::patch(ReconciliationPatch {
                status: Some(BindingStatus::Applying.into()),
                ..ReconciliationPatch::default()
            })
        )
        .unwrap(),
        WriteResult::Conflict
    );
}

#[test]
fn admission_capacity_merges_every_existing_state_and_stop_is_explicit() {
    for entry in [
        Entry::Queued { retries: 2 },
        Entry::Running {
            dirty: false,
            retries: 1,
        },
        Entry::WaitingRetry {
            retry_at: 900,
            retries: 1,
        },
        Entry::Exhausted,
    ] {
        let queue = WorkQueue::new(1, 4);
        queue.state.lock().unwrap().entries.insert(id(1), entry);
        assert_eq!(queue.enqueue(&id(2)), Err(EnqueueError::Full));
        assert_eq!(queue.enqueue(&id(1)), Ok(()));
        queue.stop();
        assert_eq!(queue.enqueue(&id(1)), Err(EnqueueError::Stopped));
    }
}

#[test]
fn admission_create_reports_saved_identity_and_policy_scope_remain_available() {
    let repo = Arc::new(ProcessLocalPapRepository::default());
    let queue = Arc::new(WorkQueue::new(1, 4));
    queue.enqueue(&id(2)).unwrap();
    let pap = pap_with(repo.clone(), queue);
    let policy = pap
        .create_policy(
            "test",
            &asc_policy_types::authoring::PolicyTemplate::PreventFileDeletion {
                files: vec!["/workspace/a".into()],
            },
        )
        .unwrap();
    let scope = pap
        .create_scope(&asc_policy_types::scope::ScopeSelector::Pid { pid: 10 })
        .unwrap();
    let saved = pap
        .create_binding(
            &policy.policy_id,
            policy.revision,
            &scope.scope_id,
            scope.revision,
        )
        .unwrap();
    let id = saved.spec.binding_id.clone();
    assert_eq!(saved.spec.binding_revision.get(), 1);
    assert_eq!(saved.status, BindingStatus::ApplyFailed);
    assert_eq!(saved.status.error, Some(EnqueueError::Full.failure()));
    assert_eq!(pap.get_binding(&id).unwrap(), saved);
    assert_eq!(repo.get_binding_state(&id).unwrap().unwrap().binding, saved);
    pap.update_scope(
        &scope.scope_id,
        &asc_policy_types::scope::ScopeSelector::Pid { pid: 11 },
    )
    .unwrap();
}

#[test]
fn admission_error_projection_sanitizes_untrusted_status_codes() {
    let mut initial = record(1);
    initial.binding.status.phase = BindingStatus::ApplyFailed;
    initial.binding.status.error = Some(Failure {
        kind: FailureKind::Rejected,
        code: "secret remote body".into(),
    });
    let repo = ProcessLocalPapRepository::with_binding_states(vec![initial]).unwrap();
    let expected = Some(Failure::new(
        FailureKind::Rejected,
        "RECONCILE_INTERNAL_ERROR",
    ));
    assert_eq!(repo.get_binding(&id(1)).unwrap().status.error, expected);
    assert_eq!(
        repo.list_bindings(1, 0).unwrap().items[0].status.error,
        expected
    );
}

#[test]
fn queue_retains_retry_progress_across_scan_and_duplicate_wakeup_but_restart_resets_it() {
    let repo = Arc::new(ProcessLocalPapRepository::with_binding_states(vec![record(1)]).unwrap());
    let client = Arc::new(Client::default());
    client
        .failures
        .lock()
        .unwrap()
        .extend((0..3).map(|_| Failure::new(FailureKind::Retryable, "RETRY")));
    let clock = Arc::new(TestClock::default());
    let core = core(repo.clone(), client.clone(), clock.clone());
    let queue = WorkQueue::new(1, 4);
    queue.enqueue(&id(1)).unwrap();
    assert_eq!(queue.take(), Some(id(1)));
    let mut schedule = queue.take_schedule(&id(1));
    assert_eq!(
        core.reconcile(&id(1), &mut schedule).unwrap(),
        Disposition::RetryAt { at: 100 }
    );
    assert_eq!(schedule.attempts_started, 1);
    queue.save_schedule(&id(1), schedule.clone());
    queue.finish(id(1), Some(100), true);
    queue.discover_many([(id(1), None)], 0);
    assert_eq!(queue.state.lock().unwrap().schedules[&id(1)], schedule);
    queue.enqueue(&id(1)).unwrap();
    assert_eq!(queue.take(), Some(id(1)));
    let mut schedule = queue.take_schedule(&id(1));
    assert_eq!(
        core.reconcile(&id(1), &mut schedule).unwrap(),
        Disposition::Skipped
    );
    assert_eq!(schedule.attempts_started, 1);
    assert_eq!(client.calls.lock().unwrap().len(), 1);
    queue.save_schedule(&id(1), schedule);
    queue.finish(id(1), Some(100), false);
    clock.0.store(100, Ordering::SeqCst);
    queue.tick(100);
    assert_eq!(queue.take(), Some(id(1)));
    let mut schedule = queue.take_schedule(&id(1));
    assert_eq!(
        core.reconcile(&id(1), &mut schedule).unwrap(),
        Disposition::RetryAt { at: 300 }
    );
    assert_eq!(schedule.attempts_started, 2);
    queue.save_schedule(&id(1), schedule);
    queue.finish(id(1), Some(300), true);
    drop(queue);

    // Rebuild only from repository facts, with neither previous count nor deadline.
    let fresh_queue = WorkQueue::new(1, 4);
    fresh_queue.discover_many([(id(1), None)], 100);
    assert_eq!(fresh_queue.take(), Some(id(1)));
    let mut fresh = fresh_queue.take_schedule(&id(1));
    assert_eq!(
        core.reconcile(&id(1), &mut fresh).unwrap(),
        Disposition::RetryAt { at: 200 }
    );
    assert_eq!(fresh.attempts_started, 1);
    assert_eq!(client.calls.lock().unwrap().len(), 3);
    let state = serde_json::to_value(repo.get_binding_state(&id(1)).unwrap().unwrap()).unwrap();
    assert!(state.get("runtime").is_none());
    assert_eq!(
        state["binding"]["status"],
        serde_json::json!({"phase":"PENDING_APPLY", "error":{"kind":"RETRYABLE", "code":"RETRY"}})
    );
}

#[test]
fn claim_clears_previous_error_and_explicit_retry_resets_retained_progress() {
    let repo = Arc::new(ProcessLocalPapRepository::with_binding_states(vec![record(1)]).unwrap());
    let client = Arc::new(Client::default());
    let clock = Arc::new(TestClock::default());
    let reconciler = core(repo.clone(), client.clone(), clock.clone());
    let mut schedule = AttemptSchedule::default();
    client
        .failures
        .lock()
        .unwrap()
        .push_back(Failure::new(FailureKind::Retryable, "RETRY"));
    reconciler.reconcile(&id(1), &mut schedule).unwrap();
    let failed = repo.get_binding(&id(1)).unwrap();
    assert!(failed.status.error.is_some());
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    *client.gate.lock().unwrap() = Some((entered_tx, release_rx));
    clock.0.store(100, Ordering::SeqCst);
    let task = thread::spawn(move || {
        reconciler.reconcile(&id(1), &mut schedule).unwrap();
        schedule
    });
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let running = repo.get_binding(&id(1)).unwrap();
    assert_eq!(running.status.phase, BindingStatus::Applying);
    assert_eq!(running.status.error, None);
    release_tx.send(()).unwrap();
    let mut schedule = task.join().unwrap();
    assert_eq!(schedule.attempts_started, 2);

    // A new explicit request clears status.error; even a retained old context resets.
    let current = repo.get_binding_state(&id(1)).unwrap().unwrap();
    let mut failed = current.clone();
    failed.binding.status.phase = BindingStatus::ApplyFailed;
    failed.binding.status.error = Some(EnqueueError::Full.failure());
    repo.compare_exchange_binding_state(&current, &BindingStateWrite::new(failed))
        .unwrap();
    let pap = pap_with(repo.clone(), Arc::new(WorkQueue::new(1, 4)));
    let spec = current.binding.spec;
    pap.update_binding(
        &id(1),
        &spec.policy.policy_id,
        spec.policy.revision,
        &spec.scope.scope_id,
        spec.scope.revision,
    )
    .unwrap();
    core(repo.clone(), client, clock)
        .reconcile(&id(1), &mut schedule)
        .unwrap();
    assert_eq!(schedule.attempts_started, 1);
    assert_eq!(
        repo.get_binding(&id(1)).unwrap().status.phase,
        BindingStatus::Ready
    );
}

#[test]
fn readiness_distinguishes_capacity_from_stopped_and_fatal() {
    for fatal in [false, true] {
        let queue = WorkQueue::new(1, 4);
        queue.enqueue(&id(1)).unwrap();
        assert_eq!(queue.check_ready(), Ok(()));
        assert_eq!(queue.enqueue(&id(2)), Err(EnqueueError::Full));
        if fatal {
            queue.fail();
        } else {
            queue.stop();
        }
        assert_eq!(queue.check_ready(), Err(PapError::Unavailable));
    }
}
