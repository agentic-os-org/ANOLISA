//! Exercise storage continuations with real `SQLite` and runtime scheduling, without PAP wakeups.
use super::*;
use asc_foundation_types::ResourceId;
use asc_pap::BindingReconcileEnqueuer;
use asc_pcp::{BindingReconciler, RetryPolicy};
use asc_policy_repository::{
    BindingReconcileCatalog, BindingStateSnapshot, BindingStateWrite, ReconcileCandidate,
    StoreError, WriteResult,
};
use asc_policy_repository_sqlite::SqlitePolicyRepository;
use asc_policy_runtime::reconciliation::{MonotonicClock, ReconciliationRuntime, RuntimeConfig};
use asc_policy_types::scope::PolicyReference;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Clone, Copy, Debug)]
enum Boundary {
    Claim,
    Registration,
    Result,
    Removal,
}

struct Fault {
    inner: Arc<SqlitePolicyRepository>,
    binding: ResourceId,
    boundary: Boundary,
    committed: bool,
    repaired: AtomicBool,
    hits: AtomicUsize,
    writes: Mutex<Vec<uuid::Uuid>>,
}
impl BindingStateRepository for Fault {
    fn check_writable(&self) -> Result<(), StoreError> {
        self.inner.check_writable()
    }
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
        let selected = match (&self.boundary, &write.next) {
            (Boundary::Removal, None) => true,
            (Boundary::Claim, Some(patch)) => patch
                .status
                .as_ref()
                .is_some_and(|s| s.phase == BindingStatus::Applying),
            (Boundary::Result, Some(patch)) => patch
                .status
                .as_ref()
                .is_some_and(|s| s.phase == BindingStatus::Ready),
            (Boundary::Registration, Some(patch)) => {
                patch.status.is_none()
                    && patch
                        .deployments
                        .as_ref()
                        .is_some_and(|ds| ds.iter().any(|d| d.presence == Presence::Unknown))
            }
            _ => false,
        };
        if selected
            && expected.binding.spec.binding_id == self.binding
            && !self.repaired.load(Ordering::SeqCst)
        {
            // This models the port's two possible outcomes, not a physical disk fault.
            if self.committed {
                self.inner.compare_exchange_binding_state(expected, write)?;
            }
            self.writes.lock().unwrap().push(write.write_id);
            self.hits.fetch_add(1, Ordering::SeqCst);
            return Err(if self.committed {
                StoreError::OutcomeUnknown
            } else {
                StoreError::Io
            });
        }
        self.inner.compare_exchange_binding_state(expected, write)
    }
}
impl BindingReconcileCatalog for Fault {
    fn scan_reconciliation(
        &self,
        after: Option<&ResourceId>,
        limit: usize,
    ) -> Result<Vec<ReconcileCandidate>, StoreError> {
        self.inner.scan_reconciliation(after, limit)
    }
}

struct RetryingClient {
    inner: Client,
    binding: String,
    failures: AtomicUsize,
    delete_failures: AtomicUsize,
}
impl RetryingClient {
    fn apply(&self, prepared: &PreparedApply) -> DeploymentReport {
        let mut report = self.inner.create(prepared);
        if prepared.target.id == self.binding
            && self
                .failures
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                    left.checked_sub(1)
                })
                .is_ok()
        {
            report.observations[0].presence = Presence::Unknown;
            report.error = Some(Failure::new(
                asc_pcp::FailureKind::Retryable,
                "TEST_RESPONSE_LOST",
            ));
        }
        report
    }
}
impl TargetDeploymentClient for RetryingClient {
    fn prepare_apply(&self, plan: &TargetBindingPlan) -> Result<PreparedApply, Failure> {
        self.inner.prepare_apply(plan)
    }
    fn create(&self, prepared: &PreparedApply) -> DeploymentReport {
        self.apply(prepared)
    }
    fn update(&self, previous: &[TargetRef], prepared: &PreparedApply) -> DeploymentReport {
        assert!(previous.is_empty());
        self.apply(prepared)
    }
    fn delete(&self, targets: &[TargetRef]) -> DeploymentReport {
        let mut report = self.inner.delete(targets);
        if targets.iter().any(|target| target.id == self.binding)
            && self
                .delete_failures
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                    left.checked_sub(1)
                })
                .is_ok()
        {
            report.observations[0].presence = Presence::Unknown;
            report.error = Some(Failure::new(
                asc_pcp::FailureKind::Retryable,
                "TEST_DELETE_RESPONSE_LOST",
            ));
        }
        report
    }
}

fn start<R>(
    repository: Arc<R>,
    client: Arc<dyn TargetDeploymentClient>,
    automatic_retries: u32,
) -> ReconciliationRuntime
where
    R: BindingStateRepository + BindingReconcileCatalog + 'static,
{
    let core = BindingReconciler::new(
        repository.clone(),
        Arc::new(|binding: &PreparedBinding| {
            asc_policy_adapter_agentsight::AgentSightAdapter.translate(binding)
        }),
        BTreeMap::from([(
            "agentsight".into(),
            Arc::new(move || Ok(client.clone() as Arc<dyn TargetDeploymentClient>))
                as Arc<dyn asc_pcp::TargetDeploymentClientFactory>,
        )]),
        "agentsight".into(),
        Arc::new(MonotonicClock::default()),
        RetryPolicy {
            max_attempts: 5,
            base_delay_ms: 1,
            max_delay_ms: 10,
        },
    )
    .unwrap();
    ReconciliationRuntime::start(
        repository,
        Arc::new(core),
        RuntimeConfig {
            workers: 2,
            tick_interval: Duration::from_millis(5),
            storage_retry: Duration::from_millis(10),
            max_auto_retries: automatic_retries,
            ..RuntimeConfig::default()
        },
    )
    .unwrap()
}

#[allow(
    clippy::too_many_lines,
    reason = "one composition matrix preserves the same setup and assertions across storage boundaries"
)]
fn scenario(
    boundary: Boundary,
    committed: bool,
    restart: bool,
    prior_failures: u32,
    notify: bool,
    delete_failures: u32,
) {
    use std::os::unix::fs::PermissionsExt;
    eprintln!(
        "storage scenario: {boundary:?}, committed={committed}, restart={restart}, prior_failures={prior_failures}, notify={notify}"
    );
    let directory = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let path = directory.path().join("policy-state.db");
    let repo = Arc::new(SqlitePolicyRepository::open(&path).unwrap());
    let fixture: PreparedBinding = serde_json::from_str(include_str!(
        "../../../../crates/asc-policy-types/tests/fixtures/prepared-binding.json"
    ))
    .unwrap();
    repo.put_policy(&fixture.policy).unwrap();
    // No enqueuer: scanner/timer recovery must stand on persisted intent alone.
    let pap = PapService::new(repo.clone()).with_scope_discovery(Arc::new(Discovery));
    let mut scopes = Vec::new();
    let mut ids = Vec::new();
    for _ in 0..2 {
        let scope = pap
            .create_scope_assignment(
                &fixture.scope.selector,
                &[PolicyReference {
                    policy_id: fixture.policy.policy_id.clone(),
                    policy_revision: fixture.policy.revision,
                }],
            )
            .unwrap();
        let accepted = repo
            .sync_scope_instances(
                &scope.scope_id,
                std::slice::from_ref(&fixture.scope.process),
            )
            .unwrap();
        ids.push(accepted[0].spec.binding_id.clone());
        scopes.push(scope);
    }
    let fault = Arc::new(Fault {
        inner: repo.clone(),
        binding: ids[0].clone(),
        boundary,
        committed,
        repaired: AtomicBool::new(false),
        hits: AtomicUsize::new(0),
        writes: Mutex::new(Vec::new()),
    });
    let client = Arc::new(RetryingClient {
        inner: Client::default(),
        binding: ids[0].to_string(),
        failures: AtomicUsize::new(prior_failures as usize),
        delete_failures: AtomicUsize::new(delete_failures as usize),
    });
    let runtime = start(fault.clone(), client.clone(), prior_failures);
    if matches!(boundary, Boundary::Removal) {
        wait(|| pap.get_binding(&ids[0]).unwrap().status == BindingStatus::Ready);
        pap.delete_scope(&scopes[0].scope_id).unwrap();
    }
    wait(|| fault.hits.load(Ordering::SeqCst) > 0);
    wait(|| pap.get_binding(&ids[1]).unwrap().status == BindingStatus::Ready);
    if !matches!(boundary, Boundary::Removal) {
        pap.delete_scope(&scopes[0].scope_id).unwrap();
        if notify {
            runtime.enqueuer().enqueue(&ids[0]).unwrap();
        }
        assert_eq!(
            pap.get_binding(&ids[0]).unwrap().status,
            BindingStatus::PendingDelete
        );
    }
    // Storage failures must retain the original write even with no auto-retry budget.
    wait(|| fault.hits.load(Ordering::SeqCst) >= 6);
    {
        let writes = fault.writes.lock().unwrap();
        assert!(writes.iter().all(|id| id == &writes[0]));
    }
    let runtime = if restart {
        runtime.shutdown().unwrap();
        drop(fault);
        drop(pap);
        drop(repo);
        let repo = Arc::new(SqlitePolicyRepository::open(&path).unwrap());
        let runtime = start(repo.clone(), client.clone(), prior_failures);
        (runtime, repo)
    } else {
        fault.repaired.store(true, Ordering::SeqCst);
        (runtime, repo)
    };
    let (runtime, repo) = runtime;
    let deadline = Instant::now() + Duration::from_secs(5);
    while let Some(state) = repo.get_binding_state(&ids[0]).unwrap() {
        assert!(
            Instant::now() < deadline,
            "cleanup stalled: {boundary:?}, committed={committed}, restart={restart}, state={state:?}, operations={:?}",
            client.inner.operations.lock().unwrap()
        );
        std::thread::yield_now();
    }
    assert!(matches!(
        repo.get_scope(&scopes[0].scope_id),
        Err(asc_pap::PapError::NotFound)
    ));
    assert_eq!(
        repo.get_binding_state(&ids[1])
            .unwrap()
            .unwrap()
            .binding
            .status
            .phase,
        BindingStatus::Ready
    );
    let pap = PapService::new(repo.clone()).with_scope_discovery(Arc::new(Discovery));
    pap.delete_scope(&scopes[1].scope_id).unwrap();
    wait(|| repo.list_scopes(10, 0).unwrap().total == 0);
    runtime.shutdown().unwrap();
    assert_eq!(repo.list_bindings(10, 0).unwrap().total, 0);
    let applied = matches!(boundary, Boundary::Result | Boundary::Removal);
    let cleanup = applied || (matches!(boundary, Boundary::Registration) && committed);
    // A live owner retains the successful DELETE result; restart loses that outcome.
    let repeated_delete = matches!(boundary, Boundary::Removal) && !committed && restart;
    let operations = client.inner.operations.lock().unwrap();
    assert_eq!(
        operations.iter().filter(|&&op| op == "create").count(),
        1 + usize::from(applied) + prior_failures as usize
    );
    assert_eq!(
        operations.iter().filter(|&&op| op == "delete").count(),
        1 + usize::from(cleanup) + usize::from(repeated_delete) + delete_failures as usize
    );
}

macro_rules! case {
    ($name:ident, $boundary:ident, $committed:literal, $restart:literal) => {
        #[test]
        fn $name() {
            scenario(Boundary::$boundary, $committed, $restart, 0, false, 0);
        }
    };
}
case!(claim_uncommitted_live, Claim, false, false);
case!(claim_uncommitted_restart, Claim, false, true);
case!(claim_committed_live, Claim, true, false);
case!(claim_committed_restart, Claim, true, true);
case!(registration_uncommitted_live, Registration, false, false);
case!(registration_uncommitted_restart, Registration, false, true);
case!(registration_committed_live, Registration, true, false);
case!(registration_committed_restart, Registration, true, true);
case!(result_uncommitted_live, Result, false, false);
case!(result_uncommitted_restart, Result, false, true);
case!(result_committed_live, Result, true, false);
case!(result_committed_restart, Result, true, true);
case!(removal_uncommitted_live, Removal, false, false);
case!(removal_uncommitted_restart, Removal, false, true);
case!(removal_committed_live, Removal, true, false);
case!(removal_committed_restart, Removal, true, true);

#[test]
fn result_uncommitted_live_with_default_retry_budget() {
    // Four earlier retryable remote outcomes consume the production queue budget.
    scenario(
        Boundary::Result,
        false,
        false,
        RuntimeConfig::default().max_auto_retries,
        false,
        0,
    );
}

#[test]
fn result_uncommitted_with_default_budget_and_restart() {
    scenario(
        Boundary::Result,
        false,
        true,
        RuntimeConfig::default().max_auto_retries,
        false,
        0,
    );
}

#[test]
fn result_uncommitted_with_default_budget_and_notification() {
    scenario(
        Boundary::Result,
        false,
        false,
        RuntimeConfig::default().max_auto_retries,
        true,
        0,
    );
}

#[test]
fn superseding_delete_has_a_fresh_budget_without_notification() {
    scenario(
        Boundary::Result,
        false,
        false,
        RuntimeConfig::default().max_auto_retries,
        false,
        1,
    );
}
