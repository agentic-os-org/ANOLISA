#[path = "admission_tests.rs"]
mod admission_tests;
use super::queue::Entry;
use super::*;
use asc_pap::{BindingReconcileEnqueuer, PapRepository, PapService};
use asc_pap_repository_memory::ProcessLocalPapRepository;
use asc_pcp::{
    DeploymentReport, Failure, FailureKind, Observation, PreparedApply, Presence, RetryPolicy,
    TargetDeploymentClient, TargetRef,
};
use asc_policy_repository::BindingStateSnapshot;
use asc_policy_types::binding::{BindingView, PreparedBinding};
use asc_policy_types::target::{TargetBindingPlan, TranslationOutcome};
use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Barrier, Mutex, mpsc};

#[path = "panic_tests.rs"]
mod panic_tests;

fn id(n: u32) -> ResourceId {
    ResourceId::new(format!("10000000-0000-4000-8000-{n:012}")).unwrap()
}
fn record(n: u32) -> BindingStateSnapshot {
    let mut spec: PreparedBinding = serde_json::from_str(include_str!(
        "../../../asc-policy-types/tests/fixtures/prepared-binding.json"
    ))
    .unwrap();
    spec.binding_id = id(n);
    spec.scope.scope_id = ResourceId::new(format!("scope-{n}")).unwrap();
    BindingStateSnapshot {
        status_version: 1,
        binding: BindingView {
            spec,
            status: (BindingStatus::PendingApply).into(),
        },
        deployments: vec![],
    }
}
fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < deadline, "condition did not converge");
        thread::yield_now();
    }
}

#[test]
fn fifo_coalesces_and_dirty_survives_notification_finish_races() {
    for _ in 0..32 {
        let q = Arc::new(WorkQueue::new(3, 4));
        let _ = q.enqueue(&id(1));
        let _ = q.enqueue(&id(1));
        let _ = q.enqueue(&id(2));
        assert_eq!(q.take(), Some(id(1)));
        let barrier = Arc::new(Barrier::new(3));
        let a = {
            let q = q.clone();
            let b = barrier.clone();
            thread::spawn(move || {
                b.wait();
                let _ = q.enqueue(&id(1));
            })
        };
        let b = {
            let q = q.clone();
            let b = barrier.clone();
            thread::spawn(move || {
                b.wait();
                q.finish(id(1), None, false);
            })
        };
        barrier.wait();
        a.join().unwrap();
        b.join().unwrap();
        let s = q.state.lock().unwrap();
        assert_eq!(s.ready, VecDeque::from([id(2), id(1)]));
        assert_eq!(s.entries.len(), 2);
        assert_eq!(s.entries[&id(1)], Entry::Queued { retries: 0 });
    }
}

#[test]
fn concurrent_takers_never_claim_one_id_twice() {
    let q = Arc::new(WorkQueue::new(2, 4));
    let _ = q.enqueue(&id(1));
    let _ = q.enqueue(&id(2));
    let barrier = Arc::new(Barrier::new(3));
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let q = q.clone();
            let b = barrier.clone();
            thread::spawn(move || {
                b.wait();
                q.take().unwrap()
            })
        })
        .collect();
    barrier.wait();
    let a = handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .collect::<Vec<_>>();
    assert_ne!(a[0], a[1]);
    assert!(q.state.lock().unwrap().ready.is_empty());
}

#[test]
fn batch_discovery_preserves_existing_work_and_applies_capacity_and_deadlines() {
    let q = WorkQueue::new(6, 0);
    let _ = q.enqueue(&id(1));
    assert_eq!(q.take(), Some(id(1)));
    q.enqueue(&id(1)).unwrap(); // Running and dirty.
    let _ = q.enqueue(&id(2));
    assert_eq!(q.take(), Some(id(2)));
    assert!(q.finish(id(2), Some(100), true));
    q.finish_terminalization(id(2), false); // Unconfirmed terminal write retains Exhausted.
    q.discover_many([(id(3), Some(200))], 100);
    let _ = q.enqueue(&id(4));
    let before = q.state.lock().unwrap().entries.clone();
    q.discover_many(
        [
            (id(1), None),
            (id(2), None),
            (id(3), None),
            (id(4), Some(300)),
            (id(5), Some(100)),
            (id(5), None),
            (id(6), Some(300)),
            (id(7), None),
        ],
        100,
    );
    let mut expected = before;
    expected.insert(id(5), Entry::Queued { retries: 0 });
    expected.insert(
        id(6),
        Entry::WaitingRetry {
            retry_at: 300,
            retries: 0,
        },
    );
    {
        let state = q.state.lock().unwrap();
        assert_eq!(state.entries, expected);
        assert_eq!(state.ready, VecDeque::from([id(4), id(5)]));
        assert_eq!(state.overflow_count, 1);
    }
    q.stop();
    q.discover_many([(id(7), None)], 300);
    let state = q.state.lock().unwrap();
    assert_eq!(state.entries, expected);
    assert_eq!(state.ready, VecDeque::from([id(4), id(5)]));
    assert_eq!(state.overflow_count, 1);
}

#[test]
fn retry_deadline_is_invalidated_by_delete_and_discovery_never_dirties() {
    let q = WorkQueue::new(2, 4);
    let _ = q.enqueue(&id(1));
    assert_eq!(q.take(), Some(id(1)));
    q.discover_many([(id(1), None)], 0);
    assert_eq!(
        q.state.lock().unwrap().entries[&id(1)],
        Entry::Running {
            dirty: false,
            retries: 0
        }
    );
    q.finish(id(1), Some(100), false);
    q.tick(99);
    assert!(q.state.lock().unwrap().ready.is_empty());
    let _ = q.enqueue(&id(1));
    q.tick(100);
    q.tick(101);
    assert_eq!(q.state.lock().unwrap().ready, VecDeque::from([id(1)]));
    q.take();
    let _ = q.enqueue(&id(1));
    q.finish(id(1), Some(1000), false);
    assert_eq!(q.state.lock().unwrap().ready, VecDeque::from([id(1)]));
}

#[test]
fn capacity_includes_running_and_waiting_and_stop_wakes_takers() {
    let q = Arc::new(WorkQueue::new(1, 4));
    let _ = q.enqueue(&id(1));
    q.take();
    let _ = q.enqueue(&id(2));
    assert!(q.state.lock().unwrap().overflow_count > 0);
    q.finish(id(1), Some(1000), false);
    let _ = q.enqueue(&id(2));
    assert_eq!(q.state.lock().unwrap().entries.len(), 1);
    let waiter = {
        let q = q.clone();
        thread::spawn(move || q.take())
    };
    q.stop();
    assert_eq!(waiter.join().unwrap(), None);
    assert!(q.check_ready().is_err());
}

#[derive(Default)]
struct TestClock(AtomicU64);
impl Clock for TestClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}
struct Client {
    calls: Mutex<Vec<String>>,
    requests: Mutex<Vec<PreparedApply>>,
    failures: Mutex<VecDeque<Failure>>,
    preparations: AtomicUsize,
    gate: Mutex<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>>,
    panic_on_create: AtomicBool,
}
impl Default for Client {
    fn default() -> Self {
        Self {
            calls: Mutex::new(vec![]),
            requests: Mutex::new(vec![]),
            failures: Mutex::new(VecDeque::new()),
            preparations: AtomicUsize::new(0),
            gate: Mutex::new(None),
            panic_on_create: AtomicBool::new(false),
        }
    }
}
impl TargetDeploymentClient for Client {
    fn prepare_apply(&self, plan: &TargetBindingPlan) -> Result<PreparedApply, Failure> {
        self.preparations.fetch_add(1, Ordering::SeqCst);
        let spec: PreparedBinding = serde_json::from_slice(&plan.content).unwrap();
        Ok(PreparedApply {
            target: TargetRef {
                route: "test".into(),
                id: spec.binding_id.to_string(),
                cleanup: vec![1],
            },
            format: "test".into(),
            content: plan.content.clone(),
        })
    }
    fn create(&self, p: &PreparedApply) -> DeploymentReport {
        self.requests.lock().unwrap().push(p.clone());
        self.calls
            .lock()
            .unwrap()
            .push(format!("apply:{}", p.target.id));
        let gate = self.gate.lock().unwrap().take();
        if let Some((entered, release)) = gate {
            entered.send(()).unwrap();
            release.recv().unwrap();
        }
        assert!(
            !self.panic_on_create.swap(false, Ordering::SeqCst),
            "scripted create panic"
        );
        let error = self.failures.lock().unwrap().pop_front();
        DeploymentReport {
            observations: vec![Observation {
                target: p.target.clone(),
                presence: if error.is_some() {
                    Presence::Unknown
                } else {
                    Presence::Present
                },
            }],
            error,
        }
    }
    fn update(&self, previous: &[TargetRef], p: &PreparedApply) -> DeploymentReport {
        assert!(previous.is_empty());
        self.create(p)
    }
    fn delete(&self, targets: &[TargetRef]) -> DeploymentReport {
        self.calls
            .lock()
            .unwrap()
            .extend(targets.iter().map(|t| format!("delete:{}", t.id)));
        DeploymentReport {
            observations: targets
                .iter()
                .map(|t| Observation {
                    target: t.clone(),
                    presence: Presence::Absent,
                })
                .collect(),
            error: None,
        }
    }
}
fn start(
    repo: Arc<ProcessLocalPapRepository>,
    client: Arc<Client>,
    clock: Arc<TestClock>,
    capacity: usize,
) -> ReconciliationRuntime {
    start_with_workers(repo, client, clock, capacity, 1)
}
fn start_with_workers(
    repo: Arc<ProcessLocalPapRepository>,
    client: Arc<Client>,
    clock: Arc<TestClock>,
    capacity: usize,
    workers: usize,
) -> ReconciliationRuntime {
    let core = core(repo.clone(), client, clock);
    ReconciliationRuntime::start(
        repo,
        core,
        RuntimeConfig {
            workers,
            capacity,
            scan_batch: 1,
            tick_interval: Duration::from_millis(1),
            ..RuntimeConfig::default()
        },
    )
    .unwrap()
}
fn core(
    repo: Arc<dyn BindingStateRepository>,
    client: Arc<Client>,
    clock: Arc<dyn Clock>,
) -> Arc<BindingReconciler> {
    let adapter = |b: &PreparedBinding| {
        Ok(TranslationOutcome::Translated(TargetBindingPlan {
            format: "test".into(),
            content: serde_json::to_vec(b).unwrap(),
        }))
    };
    Arc::new(
        BindingReconciler::new(
            repo,
            Arc::new(adapter),
            BTreeMap::from([("test".into(), {
                Arc::new(move || Ok(client.clone() as Arc<dyn TargetDeploymentClient>))
                    as Arc<dyn asc_pcp::TargetDeploymentClientFactory>
            })]),
            "test".into(),
            clock,
            RetryPolicy {
                max_attempts: 3,
                base_delay_ms: 100,
                max_delay_ms: 200,
            },
        )
        .unwrap(),
    )
}
fn status(repo: &ProcessLocalPapRepository, n: u32) -> Option<BindingStatus> {
    repo.get_binding_state(&id(n))
        .unwrap()
        .map(|r| r.binding.status.phase)
}

#[test]
fn one_worker_serves_other_bindings_during_retry_and_reprepares_on_deadline() {
    let repo = Arc::new(
        ProcessLocalPapRepository::with_binding_states(vec![record(1), record(2)]).unwrap(),
    );
    let client = Arc::new(Client::default());
    client
        .failures
        .lock()
        .unwrap()
        .push_back(Failure::new(FailureKind::Retryable, "TEST_RETRY"));
    let clock = Arc::new(TestClock::default());
    // A nonzero origin catches a scheduler that constructs its own clock.
    clock.0.store(50_000, Ordering::SeqCst);
    let service = start(repo.clone(), client.clone(), clock.clone(), 4);
    wait_until(|| status(&repo, 2) == Some(BindingStatus::Ready));
    let pending = repo.get_binding_state(&id(1)).unwrap().unwrap();
    assert_eq!(pending.binding.spec, record(1).binding.spec);
    wait_until(|| {
        service
            .queue
            .state
            .lock()
            .unwrap()
            .schedules
            .contains_key(&id(1))
    });
    assert_eq!(
        service.queue.state.lock().unwrap().schedules[&id(1)].attempts_started,
        1
    );
    assert_eq!(
        service.queue.state.lock().unwrap().schedules[&id(1)].next_attempt_at,
        Some(50_100)
    );
    assert_eq!(pending.deployments[0].presence, Presence::Unknown);
    wait_until(|| {
        service.queue.state.lock().unwrap().entries.get(&id(1))
            == Some(&Entry::WaitingRetry {
                retry_at: 50_100,
                retries: 1,
            })
    });
    service.queue.tick(50_099);
    assert_eq!(repo.get_binding_state(&id(1)).unwrap(), Some(pending));
    assert!(service.queue.state.lock().unwrap().ready.is_empty());
    clock.0.store(50_100, Ordering::SeqCst);
    wait_until(|| status(&repo, 1) == Some(BindingStatus::Ready));
    assert_eq!(client.preparations.load(Ordering::SeqCst), 3);
    assert_eq!(
        *client.calls.lock().unwrap(),
        vec![
            format!("apply:{}", id(1)),
            format!("apply:{}", id(2)),
            format!("apply:{}", id(1))
        ]
    );
    service.shutdown().unwrap();
}

#[test]
fn compensation_pages_past_capacity_without_any_notifications() {
    let mut records: Vec<_> = (1..=7).map(record).collect();
    let mut terminal = record(0);
    terminal.binding.status.phase = BindingStatus::ApplyFailed;
    records.insert(0, terminal);
    let repo = Arc::new(ProcessLocalPapRepository::with_binding_states(records).unwrap());
    let client = Arc::new(Client::default());
    let service = start(
        repo.clone(),
        client.clone(),
        Arc::new(TestClock::default()),
        1,
    );
    wait_until(|| (1..=7).all(|n| status(&repo, n) == Some(BindingStatus::Ready)));
    assert_eq!(client.calls.lock().unwrap().len(), 7);
    service.shutdown().unwrap();
}

#[test]
fn delete_admitted_during_apply_waits_for_exit_and_preserves_cleanup() {
    let repo = Arc::new(ProcessLocalPapRepository::with_binding_states(vec![record(1)]).unwrap());
    let client = Arc::new(Client::default());
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    *client.gate.lock().unwrap() = Some((entered_tx, release_rx));
    let service = start_with_workers(
        repo.clone(),
        client.clone(),
        Arc::new(TestClock::default()),
        2,
        2,
    );
    let pap = PapService::new(repo.clone())
        .with_reconcile_enqueuer(service.enqueuer())
        .with_scope_discovery(Arc::new(Discovery));
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    pap.delete_scope(&record(1).binding.spec.scope.scope_id)
        .unwrap();
    let accepted = pap.get_binding(&id(1)).unwrap();
    assert_eq!(accepted.spec, record(1).binding.spec);
    assert_eq!(accepted.status, BindingStatus::PendingDelete);
    assert_eq!(client.calls.lock().unwrap().len(), 1);
    // A second worker must be able to finish another Binding while this one
    // remains Running/dirty. No core mutex participates in this exclusion.
    let mut other = record(2).binding;
    other.spec.binding_revision = asc_foundation_types::Revision::new(1).unwrap();
    repo.put_policy(&other.spec.policy).unwrap();
    repo.put_scope(&asc_policy_types::scope::PreparedScope {
        scope_id: other.spec.scope.scope_id.clone(),
        selector: other.spec.scope.selector.clone(),
        policy_snapshots: vec![other.spec.policy.clone()],
        status: asc_policy_types::scope::ScopeStatus::Active,
    })
    .unwrap();
    repo.update_binding(None, &other).unwrap();
    service.enqueuer().enqueue(&id(2)).unwrap();
    wait_until(|| status(&repo, 2) == Some(BindingStatus::Ready));
    assert_eq!(status(&repo, 1), Some(BindingStatus::PendingDelete));
    assert_eq!(
        service.enqueuer().state.lock().unwrap().entries.get(&id(1)),
        Some(&Entry::Running {
            dirty: true,
            retries: 0
        })
    );
    assert_eq!(
        *client.calls.lock().unwrap(),
        vec![format!("apply:{}", id(1)), format!("apply:{}", id(2))]
    );
    release_tx.send(()).unwrap();
    wait_until(|| status(&repo, 1).is_none());
    assert_eq!(
        *client.calls.lock().unwrap(),
        vec![
            format!("apply:{}", id(1)),
            format!("apply:{}", id(2)),
            format!("delete:{}", id(1))
        ]
    );
    assert_eq!(client.preparations.load(Ordering::SeqCst), 2);
    service.shutdown().unwrap();
}

#[test]
fn delete_preempts_waiting_retry_without_waiting_for_clock() {
    let repo = Arc::new(ProcessLocalPapRepository::with_binding_states(vec![record(1)]).unwrap());
    let client = Arc::new(Client::default());
    client
        .failures
        .lock()
        .unwrap()
        .push_back(Failure::new(FailureKind::Retryable, "TEST_RETRY"));
    let service = start(
        repo.clone(),
        client.clone(),
        Arc::new(TestClock::default()),
        2,
    );
    wait_until(|| {
        matches!(
            service.queue.state.lock().unwrap().entries.get(&id(1)),
            Some(Entry::WaitingRetry { .. })
        )
    });
    let pap = PapService::new(repo.clone())
        .with_reconcile_enqueuer(service.enqueuer())
        .with_scope_discovery(Arc::new(Discovery));
    pap.delete_scope(&record(1).binding.spec.scope.scope_id)
        .unwrap();
    wait_until(|| status(&repo, 1).is_none());
    assert_eq!(client.preparations.load(Ordering::SeqCst), 1);
    service.shutdown().unwrap();
}

#[test]
fn shutdown_retains_actual_call_until_join_and_closes_write_admission() {
    let repo = Arc::new(ProcessLocalPapRepository::with_binding_states(vec![record(1)]).unwrap());
    let client = Arc::new(Client::default());
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    *client.gate.lock().unwrap() = Some((entered_tx, release_rx));
    let service = start(repo.clone(), client, Arc::new(TestClock::default()), 2);
    let queue = service.enqueuer();
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        service.shutdown().unwrap();
        done_tx.send(()).unwrap();
    });
    wait_until(|| !queue.is_healthy());
    assert!(done_rx.try_recv().is_err());
    assert_eq!(status(&repo, 1), Some(BindingStatus::Applying));
    release_tx.send(()).unwrap();
    done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    handle.join().unwrap();
    assert_eq!(status(&repo, 1), Some(BindingStatus::Ready));
}

#[test]
fn scope_replacement_delivers_distinct_bindings_and_cleans_owned_state() {
    let repo = Arc::new(ProcessLocalPapRepository::default());
    let client = Arc::new(Client::default());
    let service = start(
        repo.clone(),
        client.clone(),
        Arc::new(TestClock::default()),
        4,
    );
    exercise_independent_pap_crud(&repo, &service.enqueuer());
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let first: PreparedBinding = serde_json::from_slice(&requests[0].content).unwrap();
    let second: PreparedBinding = serde_json::from_slice(&requests[1].content).unwrap();
    assert_ne!(first.binding_id, second.binding_id);
    assert_ne!(first.scope.scope_id, second.scope.scope_id);
    assert_eq!(first.policy, second.policy);
    drop(requests);
    assert!(repo.list_scopes(100, 0).unwrap().items.is_empty());
    service.shutdown().unwrap();
}

#[test]
fn timer_panic_closes_admission_and_shutdown_observes_failure() {
    struct PanickingClock;
    impl Clock for PanickingClock {
        fn now_ms(&self) -> u64 {
            panic!("scripted timer failure")
        }
    }
    // No candidates or enqueues: only the timer reads the failing clock.
    let repo = Arc::new(ProcessLocalPapRepository::default());
    let runtime = ReconciliationRuntime::start(
        repo.clone(),
        core(
            repo.clone(),
            Arc::new(Client::default()),
            Arc::new(PanickingClock),
        ),
        RuntimeConfig::default(),
    )
    .unwrap();
    let queue = runtime.enqueuer();
    wait_until(|| queue.has_failed());
    assert!(queue.check_ready().is_err());
    assert!(queue.state.lock().unwrap().entries.is_empty());
    let _ = queue.enqueue(&id(1));
    assert_eq!(queue.take(), None);
    assert_eq!(repo.get_binding_state(&id(1)).unwrap(), None);
    assert_eq!(runtime.shutdown(), Err(StoreError::Unavailable));
}

#[test]
fn pap_notification_observes_committed_state_and_failed_admission_never_notifies() {
    struct Observer {
        repo: Arc<ProcessLocalPapRepository>,
        seen: Mutex<Vec<BindingView>>,
    }
    impl BindingReconcileEnqueuer for Observer {
        fn check_ready(&self) -> Result<(), asc_pap::PapError> {
            Ok(())
        }
        fn enqueue(&self, id: &ResourceId) -> Result<(), asc_pap::EnqueueError> {
            self.seen
                .lock()
                .unwrap()
                .push(self.repo.get_binding(id).unwrap());
            Ok(())
        }
    }
    let repo = Arc::new(ProcessLocalPapRepository::default());
    let observer = Arc::new(Observer {
        repo: repo.clone(),
        seen: Mutex::new(vec![]),
    });
    let pap = PapService::new(repo.clone())
        .with_reconcile_enqueuer(observer.clone())
        .with_scope_discovery(Arc::new(Discovery));
    let spec = record(1).binding.spec;
    let policy = pap.create_policy("test", &spec.policy.template).unwrap();
    let refs = [asc_policy_types::scope::PolicyReference {
        policy_id: policy.policy_id.clone(),
        policy_revision: policy.revision,
    }];
    let scope = pap
        .create_scope_assignment(&spec.scope.selector, &refs)
        .unwrap();
    assert!(
        asc_pap::ScopeBindingSink::sync_instances(
            &pap,
            &id(999),
            std::slice::from_ref(&spec.scope.process)
        )
        .is_err()
    );
    assert!(observer.seen.lock().unwrap().is_empty());
    asc_pap::ScopeBindingSink::sync_instances(
        &pap,
        &scope.scope_id,
        std::slice::from_ref(&spec.scope.process),
    )
    .unwrap();
    let created = repo.list_bindings(10, 0).unwrap().items.remove(0);
    asc_pap::ScopeBindingSink::sync_instances(&pap, &scope.scope_id, &[spec.scope.process])
        .unwrap();
    pap.delete_scope(&scope.scope_id).unwrap();
    let deleted = pap.get_binding(&created.spec.binding_id).unwrap();
    pap.delete_scope(&scope.scope_id).unwrap();
    assert_eq!(*observer.seen.lock().unwrap(), vec![created, deleted]);
}

#[test]
fn waiting_retry_releases_worker_without_closing_admission() {
    let q = WorkQueue::new(2, 4);
    let _ = q.enqueue(&id(1));
    let _ = q.enqueue(&id(2));
    assert_eq!(q.take(), Some(id(1)));
    q.finish(id(1), Some(100), false);
    assert!(q.check_ready().is_ok());
    assert_eq!(q.take(), Some(id(2)));
    q.finish(id(2), None, false);
    assert!(q.check_ready().is_ok());
    q.tick(100);
    assert_eq!(q.take(), Some(id(1)));
    q.finish(id(1), None, false);
    assert!(q.check_ready().is_ok());
    assert!(q.state.lock().unwrap().entries.is_empty());
}

#[test]
fn scan_failure_degrades_health_without_closing_binding_admission() {
    let q = WorkQueue::new(1, 4);
    q.state.lock().unwrap().scan_failed = true;
    assert!(!q.is_healthy());
    assert_eq!(q.check_ready(), Ok(()));
    let _ = q.enqueue(&id(1));
    assert_eq!(q.take(), Some(id(1)));
    q.finish(id(1), None, false);
    q.state.lock().unwrap().scan_failed = false;
    assert!(q.is_healthy());
    q.stop();
    assert_eq!(q.check_ready(), Err(asc_pap::PapError::Unavailable));
}

struct RepeatedOutcome {
    core: Arc<BindingReconciler>,
    result: Result<Disposition, StoreError>,
    calls: Mutex<Vec<ResourceId>>,
}
impl ReconcileAttempt for RepeatedOutcome {
    fn clock(&self) -> Arc<dyn Clock> {
        self.core.clock()
    }
    fn reconcile(
        &self,
        binding_id: &ResourceId,
        schedule: &mut AttemptSchedule,
    ) -> Result<Disposition, StoreError> {
        self.calls.lock().unwrap().push(binding_id.clone());
        if binding_id == &id(1) {
            self.result.clone()
        } else {
            self.core.reconcile(binding_id, schedule)
        }
    }
}

#[test]
fn automatic_retries_wait_and_stop_at_budget_without_blocking_other_bindings() {
    for result in [
        Ok(Disposition::Superseded),
        // Even a deadline that passed during bookkeeping must yield to a timer.
        Ok(Disposition::RetryAt { at: 0 }),
        Err(StoreError::Contended),
        Err(StoreError::Invalid),
    ] {
        let delay = if matches!(result, Ok(Disposition::RetryAt { .. })) {
            1
        } else {
            1000
        };
        let repo = Arc::new(
            ProcessLocalPapRepository::with_binding_states(vec![record(1), record(2)]).unwrap(),
        );
        let clock = Arc::new(TestClock::default());
        let attempt = Arc::new(RepeatedOutcome {
            core: core(repo.clone(), Arc::new(Client::default()), clock.clone()),
            result,
            calls: Mutex::new(vec![]),
        });
        let runtime = ReconciliationRuntime::start(
            repo.clone(),
            attempt.clone(),
            RuntimeConfig {
                workers: 1,
                max_auto_retries: 2,
                // Drive deadlines explicitly so compensation cannot race PAP's
                // retry notification and add an unrelated scheduling series.
                tick_interval: Duration::from_secs(60),
                ..RuntimeConfig::default()
            },
        )
        .unwrap();
        let q = runtime.enqueuer();
        for retry in 1..=2 {
            let deadline = u64::from(retry) * delay;
            wait_until(|| {
                q.state.lock().unwrap().entries.get(&id(1))
                    == Some(&Entry::WaitingRetry {
                        retry_at: deadline,
                        retries: retry,
                    })
                    && status(&repo, 2) == Some(BindingStatus::Ready)
            });
            q.tick(deadline - 1);
            q.discover_many([(id(1), None)], deadline);
            assert!(q.state.lock().unwrap().ready.is_empty());
            assert_eq!(repo.get_binding_state(&id(1)).unwrap(), Some(record(1)));
            assert_eq!(
                attempt
                    .calls
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|value| **value == id(1))
                    .count(),
                retry as usize
            );
            clock.0.store(deadline, Ordering::SeqCst);
            q.tick(deadline);
        }
        wait_until(|| {
            status(&repo, 1) == Some(BindingStatus::ApplyFailed)
                && q.state.lock().unwrap().entries.is_empty()
        });
        assert_eq!(
            *attempt.calls.lock().unwrap(),
            vec![id(1), id(2), id(1), id(1)]
        );
        let mut expected = record(1);
        expected.status_version += 1;
        expected.binding.status.phase = BindingStatus::ApplyFailed;
        expected.binding.status.error = Some(Failure::new(
            FailureKind::Rejected,
            "RECONCILE_RETRY_EXHAUSTED",
        ));
        assert_eq!(repo.get_binding_state(&id(1)).unwrap(), Some(expected));
        assert!(q.state.lock().unwrap().schedules.is_empty());
        exercise_independent_pap_crud(&repo, &q);
        assert!(q.is_healthy());

        // An explicit retry clears Failed before starting a new scheduling series.
        retry_failed_binding(&repo, &q);
        wait_until(|| {
            q.state.lock().unwrap().entries.get(&id(1))
                == Some(&Entry::WaitingRetry {
                    retry_at: 3 * delay,
                    retries: 1,
                })
        });
        assert_eq!(
            attempt
                .calls
                .lock()
                .unwrap()
                .iter()
                .filter(|value| **value == id(1))
                .count(),
            4
        );
        runtime.shutdown().unwrap();
    }
}

#[test]
fn new_notifications_reset_queue_budget_and_preempt_waiting_or_exhaustion() {
    let q = WorkQueue::new(1, 1);
    let _ = q.enqueue(&id(1));
    assert_eq!(q.take(), Some(id(1)));
    assert!(!q.finish(id(1), Some(100), true));
    q.tick(100);
    // Notification of a newly admitted intent coalesces with the queued retry.
    let _ = q.enqueue(&id(1));
    assert_eq!(q.state.lock().unwrap().ready, VecDeque::from([id(1)]));
    assert_eq!(q.take(), Some(id(1)));
    assert!(!q.finish(id(1), Some(200), true));
    q.tick(200);
    assert_eq!(q.take(), Some(id(1)));
    let _ = q.enqueue(&id(1));
    // Dirty is a new notification, so the exhausted old budget cannot suppress it.
    assert!(!q.finish(id(1), Some(300), true));
    assert_eq!(q.take(), Some(id(1)));
    assert!(!q.finish(id(1), Some(300), true));
    let _ = q.enqueue(&id(1));
    assert_eq!(q.take(), Some(id(1)));
    assert!(!q.finish(id(1), Some(400), true));
    q.tick(400);
    assert_eq!(q.take(), Some(id(1)));
    assert!(q.finish(id(1), Some(500), true));
    q.finish_terminalization(id(1), false);
    q.discover_many([(id(1), None)], 1000);
    q.tick(1000);
    assert!(q.state.lock().unwrap().ready.is_empty());
    let _ = q.enqueue(&id(1));
    assert_eq!(q.take(), Some(id(1)));
    assert!(!q.finish(id(1), None, false));
    assert!(q.state.lock().unwrap().entries.is_empty());
}

#[test]
fn zero_retry_budget_stops_first_failure_and_submillisecond_delay_is_rejected() {
    let q = WorkQueue::new(1, 0);
    let _ = q.enqueue(&id(1));
    assert_eq!(q.take(), Some(id(1)));
    assert!(q.finish(id(1), Some(100), true));
    q.finish_terminalization(id(1), false);
    assert_eq!(
        q.state.lock().unwrap().entries.get(&id(1)),
        Some(&Entry::Exhausted)
    );
    let repo = Arc::new(ProcessLocalPapRepository::default());
    let clock = Arc::new(TestClock::default());
    assert!(matches!(
        ReconciliationRuntime::start(
            repo.clone(),
            core(repo, Arc::new(Client::default()), clock.clone()),
            RuntimeConfig {
                storage_retry: Duration::from_nanos(1),
                ..RuntimeConfig::default()
            },
        ),
        Err(StoreError::Invalid)
    ));
}

#[test]
fn binding_errors_retry_without_blocking_other_bindings_or_pap_writes() {
    struct FailOnce {
        core: Arc<BindingReconciler>,
        error: StoreError,
        calls: Mutex<Vec<ResourceId>>,
    }
    impl ReconcileAttempt for FailOnce {
        fn clock(&self) -> Arc<dyn Clock> {
            self.core.clock()
        }
        fn reconcile(
            &self,
            id: &ResourceId,
            schedule: &mut AttemptSchedule,
        ) -> Result<Disposition, StoreError> {
            let mut calls = self.calls.lock().unwrap();
            let first = calls.is_empty();
            calls.push(id.clone());
            drop(calls);
            if first {
                Err(self.error)
            } else {
                self.core.reconcile(id, schedule)
            }
        }
    }
    for error in [
        StoreError::Unavailable,
        StoreError::Invalid,
        StoreError::Contended,
    ] {
        let repo = Arc::new(
            ProcessLocalPapRepository::with_binding_states(vec![record(1), record(2)]).unwrap(),
        );
        let clock = Arc::new(TestClock::default());
        let client = Arc::new(Client::default());
        let attempt = Arc::new(FailOnce {
            core: core(repo.clone(), client.clone(), clock.clone()),
            error,
            calls: Mutex::new(vec![]),
        });
        let runtime = ReconciliationRuntime::start(
            repo.clone(),
            attempt.clone(),
            RuntimeConfig {
                workers: 1,
                tick_interval: Duration::from_millis(1),
                ..RuntimeConfig::default()
            },
        )
        .unwrap();
        let q = runtime.enqueuer();
        wait_until(|| {
            q.state.lock().unwrap().entries.get(&id(1))
                == Some(&Entry::WaitingRetry {
                    retry_at: 1000,
                    retries: u32::from(error != StoreError::Unavailable),
                })
                && status(&repo, 2) == Some(BindingStatus::Ready)
        });
        assert_eq!(*attempt.calls.lock().unwrap(), vec![id(1), id(2)]);
        assert_eq!(repo.get_binding_state(&id(1)).unwrap(), Some(record(1)));
        assert_eq!(q.check_ready(), Ok(()));
        assert!(q.is_healthy());

        exercise_independent_pap_crud(&repo, &q);
        // PAP and the other workers made progress while A's retry stayed pending.
        assert_eq!(repo.get_binding_state(&id(1)).unwrap(), Some(record(1)));
        assert_eq!(
            q.state.lock().unwrap().entries[&id(1)],
            Entry::WaitingRetry {
                retry_at: 1000,
                retries: u32::from(error != StoreError::Unavailable)
            }
        );
        clock.0.store(1000, Ordering::SeqCst);
        wait_until(|| status(&repo, 1) == Some(BindingStatus::Ready));
        assert_eq!(
            attempt
                .calls
                .lock()
                .unwrap()
                .iter()
                .filter(|value| **value == id(1))
                .count(),
            2
        );
        runtime.shutdown().unwrap();
    }
}

fn exercise_independent_pap_crud(repo: &Arc<ProcessLocalPapRepository>, q: &Arc<WorkQueue>) {
    let pap = PapService::new(repo.clone())
        .with_reconcile_enqueuer(q.clone())
        .with_scope_discovery(Arc::new(Discovery));
    let policy = pap
        .create_policy("independent", &record(1).binding.spec.policy.template)
        .unwrap();
    let refs = [asc_policy_types::scope::PolicyReference {
        policy_id: policy.policy_id.clone(),
        policy_revision: policy.revision,
    }];
    let mut scopes = Vec::new();
    for pid in [99, 100] {
        let scope = pap
            .create_scope_assignment(&asc_policy_types::scope::ScopeSelector::Pid { pid }, &refs)
            .unwrap();
        let mut instance = record(1).binding.spec.scope.process;
        instance.pid = pid;
        asc_pap::ScopeBindingSink::sync_instances(&pap, &scope.scope_id, &[instance]).unwrap();
        wait_until(|| {
            pap.list_bindings(100, 0).unwrap().items.iter().any(|b| {
                b.spec.scope.scope_id == scope.scope_id && b.status == BindingStatus::Ready
            })
        });
        scopes.push(scope);
    }
    let policy = pap
        .update_policy(&policy.policy_id, "renamed", &policy.template)
        .unwrap();
    pap.delete_policy_revision(&policy.policy_id, policy.revision)
        .unwrap();
    for scope in scopes {
        pap.delete_scope(&scope.scope_id).unwrap();
        wait_until(|| {
            matches!(
                pap.get_scope(&scope.scope_id),
                Err(asc_pap::PapError::NotFound)
            )
        });
    }
}

#[path = "termination_tests.rs"]
mod termination_tests;

fn retry_failed_binding(repo: &Arc<ProcessLocalPapRepository>, queue: &Arc<WorkQueue>) {
    let pap = PapService::new(repo.clone())
        .with_reconcile_enqueuer(queue.clone())
        .with_scope_discovery(Arc::new(Discovery));
    pap.retry_scope(&record(1).binding.spec.scope.scope_id)
        .unwrap();
}

struct Discovery;
impl asc_pap::ScopeDiscovery for Discovery {
    fn start(&self, _: &asc_pap::ScopeDiscoverySeed) -> Result<(), asc_pap::PapError> {
        Ok(())
    }
    fn stop(&self, _: &asc_foundation_types::ResourceId) -> Result<(), asc_pap::PapError> {
        Ok(())
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one fault matrix compares ownership, versions, budgets and remote effects across the same recovery sequence"
)]
fn storage_backoff_retains_original_write_and_does_not_spend_retry_budget() {
    struct Fault {
        inner: Arc<ProcessLocalPapRepository>,
        error: StoreError,
        finish: bool,
        repaired: AtomicBool,
        writes: Mutex<Vec<String>>,
    }
    impl BindingStateRepository for Fault {
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
            let selected = expected.binding.spec.binding_id == id(1)
                && write.next.as_ref().is_some_and(|p| {
                    p.status.as_ref().is_some_and(|s| {
                        s.phase
                            == if self.finish {
                                BindingStatus::Ready
                            } else {
                                BindingStatus::Applying
                            }
                    })
                });
            if selected {
                self.writes.lock().unwrap().push(write.write_id.to_string());
                if !self.repaired.load(Ordering::SeqCst) {
                    if self.error == StoreError::OutcomeUnknown {
                        self.inner.compare_exchange_binding_state(expected, write)?;
                    }
                    return Err(self.error);
                }
            }
            self.inner.compare_exchange_binding_state(expected, write)
        }
    }
    for error in [
        StoreError::Busy,
        StoreError::Full,
        StoreError::ReadOnly,
        StoreError::Io,
        StoreError::OutcomeUnknown,
    ] {
        for finish in [false, true] {
            let inner = Arc::new(
                ProcessLocalPapRepository::with_binding_states(vec![record(1), record(2)]).unwrap(),
            );
            let repo = Arc::new(Fault {
                inner: inner.clone(),
                error,
                finish,
                repaired: AtomicBool::new(false),
                writes: Mutex::new(Vec::new()),
            });
            let clock = Arc::new(TestClock::default());
            let client = Arc::new(Client::default());
            let core = core(repo.clone(), client.clone(), clock.clone());
            let queue = WorkQueue::new(2, 0);
            queue.enqueue(&id(1)).unwrap();
            queue.enqueue(&id(2)).unwrap();
            for expected in [id(1), id(2)] {
                assert_eq!(queue.take(), Some(expected.clone()));
                run_attempt(
                    repo.as_ref(),
                    core.as_ref(),
                    clock.as_ref(),
                    &queue,
                    expected,
                    Duration::from_millis(1000),
                );
            }
            assert_eq!(status(&inner, 2), Some(BindingStatus::Ready));
            for tick in 1..=8 {
                let now = tick * 1000;
                clock.0.store(now, Ordering::SeqCst);
                queue.tick(now);
                assert_eq!(queue.take(), Some(id(1)));
                run_attempt(
                    repo.as_ref(),
                    core.as_ref(),
                    clock.as_ref(),
                    &queue,
                    id(1),
                    Duration::from_millis(1000),
                );
                let state = queue.state.lock().unwrap();
                assert_eq!(
                    state.entries.get(&id(1)),
                    Some(&Entry::WaitingRetry {
                        retry_at: now + 1000,
                        retries: 0
                    })
                );
                let schedule = &state.schedules[&id(1)];
                assert!(schedule.has_pending_storage());
                assert_eq!(schedule.attempts_started, u32::from(finish));
            }
            let writes = repo.writes.lock().unwrap().clone();
            assert!(writes.iter().all(|id| *id == writes[0]));
            repo.repaired.store(true, Ordering::SeqCst);
            for _ in 0..3 {
                let deadline = {
                    let state = queue.state.lock().unwrap();
                    match state.entries.get(&id(1)) {
                        Some(Entry::WaitingRetry { retry_at, .. }) => *retry_at,
                        None => break,
                        other => panic!("unexpected queue state {other:?}"),
                    }
                };
                clock.0.store(deadline, Ordering::SeqCst);
                queue.tick(deadline);
                assert_eq!(queue.take(), Some(id(1)));
                run_attempt(
                    repo.as_ref(),
                    core.as_ref(),
                    clock.as_ref(),
                    &queue,
                    id(1),
                    Duration::from_millis(1000),
                );
            }
            assert_eq!(status(&inner, 1), Some(BindingStatus::Ready));
            assert_eq!(
                client
                    .calls
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|s| **s == format!("apply:{}", id(1)))
                    .count(),
                1
            );
        }
    }
}
