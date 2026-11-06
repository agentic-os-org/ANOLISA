#[cfg(test)]
include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/policy.rs"
));

use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{JoinHandle, spawn};
use std::time::Duration;

use asc_foundation_types::{ResourceId, Revision};
use asc_pap::{Page, PapError, PapRepository, PapService, PolicyRevisionState};
use asc_policy_repository::{
    BindingStateRepository, BindingStateSnapshot, BindingStateWrite, WriteResult,
};
use asc_policy_repository_sqlite::SqlitePolicyRepository;
use asc_policy_types::authoring::PolicySource;
use asc_policy_types::binding::{BindingStatus, BindingView, PreparedBinding};
use asc_policy_types::policy::PreparedPolicy;
use asc_policy_types::scope::{PreparedScope, ScopeSelector};

const COMPLETE_BINDING: &str =
    include_str!("../../asc-policy-types/tests/fixtures/prepared-binding.json");
const WAIT: Duration = Duration::from_secs(5);
const SETTLE: Duration = Duration::from_millis(300);
// Exceeds every observation made while a gate is held, so only a missing release can expire it.
const GATE_WAIT: Duration = Duration::from_secs(30);

struct BoundaryGate {
    entered: mpsc::Sender<()>,
    resume: mpsc::Receiver<()>,
}

impl BoundaryGate {
    fn new() -> (Self, mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (entered, observed) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        (Self { entered, resume }, observed, release)
    }

    fn wait(self) {
        self.entered.send(()).unwrap();
        self.resume
            .recv_timeout(GATE_WAIT)
            .expect("gate was not released");
    }
}

struct FakeRepository {
    inner: SqlitePolicyRepository,
    failure_write_mode: std::sync::atomic::AtomicUsize,
    policy_read_override: Mutex<Option<PolicyRevisionState>>,
    scope_cleanup_failure: std::sync::atomic::AtomicUsize,
    scope_commit_failure: std::sync::atomic::AtomicUsize,
    scope_read_gate: Mutex<Option<BoundaryGate>>,
    scope_scan_gate: Mutex<Option<BoundaryGate>>,
}

impl Default for FakeRepository {
    fn default() -> Self {
        Self {
            inner: SqlitePolicyRepository::temporary().unwrap(),
            failure_write_mode: std::sync::atomic::AtomicUsize::default(),
            policy_read_override: Mutex::default(),
            scope_cleanup_failure: std::sync::atomic::AtomicUsize::default(),
            scope_commit_failure: std::sync::atomic::AtomicUsize::default(),
            scope_read_gate: Mutex::default(),
            scope_scan_gate: Mutex::default(),
        }
    }
}

impl FakeRepository {
    fn binding_state(&self, id: &ResourceId) -> BindingStateSnapshot {
        self.inner.get_binding_state(id).unwrap().unwrap()
    }

    // Set up worker-owned state through the same aggregate CAS used by the reconciler.
    fn transition(&self, id: &ResourceId, status: BindingStatus) {
        let current = self.binding_state(id);
        current
            .binding
            .status
            .phase
            .validate_successor(status)
            .unwrap();
        let mut next = current.clone();
        next.binding.status.phase = status;
        assert!(matches!(
            self.inner
                .compare_exchange_binding_state(&current, &BindingStateWrite::new(next)),
            Ok(WriteResult::Applied(_))
        ));
    }
}

impl PapRepository for FakeRepository {
    fn put_policy(&self, policy: &PreparedPolicy) -> Result<PreparedPolicy, PapError> {
        self.inner.put_policy(policy)
    }

    fn get_policy_revision_state(
        &self,
        id: &ResourceId,
    ) -> Result<Option<PolicyRevisionState>, PapError> {
        if let Some(result) = self.policy_read_override.lock().unwrap().take() {
            return Ok(Some(result));
        }
        self.inner.get_policy_revision_state(id)
    }

    fn get_policy(&self, id: &ResourceId, revision: Revision) -> Result<PreparedPolicy, PapError> {
        self.inner.get_policy(id, revision)
    }

    fn current_policies(&self, ids: &[ResourceId]) -> Result<Vec<PreparedPolicy>, PapError> {
        self.inner.current_policies(ids)
    }

    fn list_policies(&self, limit: u32, offset: u32) -> Result<Page<PreparedPolicy>, PapError> {
        self.inner.list_policies(limit, offset)
    }

    fn delete_policy_revision(
        &self,
        id: &ResourceId,
        revision: Revision,
    ) -> Result<PreparedPolicy, PapError> {
        self.inner.delete_policy_revision(id, revision)
    }

    fn put_scope(&self, scope: &PreparedScope) -> Result<PreparedScope, PapError> {
        let mode = self
            .scope_commit_failure
            .load(std::sync::atomic::Ordering::SeqCst);
        if mode == 1 {
            return Err(PapError::Persistence);
        }
        let saved = self.inner.put_scope(scope)?;
        if mode == 2 {
            return Err(PapError::Persistence);
        }
        Ok(saved)
    }

    fn get_scope(&self, id: &ResourceId) -> Result<PreparedScope, PapError> {
        let scope = self.inner.get_scope(id);
        let gate = self.scope_read_gate.lock().unwrap().take();
        if let Some(gate) = gate {
            gate.wait();
        }
        scope
    }

    fn list_scopes(&self, limit: u32, offset: u32) -> Result<Page<PreparedScope>, PapError> {
        self.inner.list_scopes(limit, offset)
    }

    fn scope_discovery_seed(
        &self,
        id: &ResourceId,
    ) -> Result<asc_pap::ScopeDiscoverySeed, PapError> {
        self.inner.scope_discovery_seed(id)
    }
    fn scan_scopes(
        &self,
        after: Option<&ResourceId>,
        limit: usize,
    ) -> Result<Vec<PreparedScope>, PapError> {
        let scopes = self.inner.scan_scopes(after, limit);
        let gate = self.scope_scan_gate.lock().unwrap().take();
        if let Some(gate) = gate {
            gate.wait();
        }
        scopes
    }
    fn begin_scope_delete(&self, id: &ResourceId) -> Result<Option<PreparedScope>, PapError> {
        if self
            .scope_cleanup_failure
            .load(std::sync::atomic::Ordering::SeqCst)
            == 1
        {
            return Err(PapError::Persistence);
        }
        self.inner.begin_scope_delete(id)
    }
    fn finish_scope_discovery(
        &self,
        id: &ResourceId,
    ) -> Result<Vec<asc_policy_repository::BindingIntentReceipt>, PapError> {
        if self
            .scope_cleanup_failure
            .load(std::sync::atomic::Ordering::SeqCst)
            == 2
        {
            return Err(PapError::Persistence);
        }
        self.inner.finish_scope_discovery(id)
    }
    fn scope_execution_ready_to_release(&self, id: &ResourceId) -> Result<bool, PapError> {
        if self
            .scope_cleanup_failure
            .load(std::sync::atomic::Ordering::SeqCst)
            == 3
        {
            return Err(PapError::Persistence);
        }
        self.inner.scope_execution_ready_to_release(id)
    }
    fn finish_scope_execution(&self, id: &ResourceId) -> Result<(), PapError> {
        self.inner.finish_scope_execution(id)
    }
    fn sync_scope_instances(
        &self,
        id: &ResourceId,
        instances: &[asc_policy_types::process_discovery::ProcessIdentity],
    ) -> Result<Vec<asc_policy_repository::BindingIntentReceipt>, PapError> {
        self.inner.sync_scope_instances(id, instances)
    }
    fn retry_scope(
        &self,
        id: &ResourceId,
    ) -> Result<Vec<asc_policy_repository::BindingIntentReceipt>, PapError> {
        self.inner.retry_scope(id)
    }
    fn fail_pending_binding(
        &self,
        expected: &asc_policy_repository::BindingIntentReceipt,
        reason: asc_pap::EnqueueError,
    ) -> Result<bool, PapError> {
        match self
            .failure_write_mode
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            1 => return Err(PapError::Persistence),
            2 => return Ok(false),
            3 => {
                self.failure_write_mode
                    .store(4, std::sync::atomic::Ordering::SeqCst);
                return Ok(false);
            }
            _ => {}
        }
        self.inner.fail_pending_binding(expected, reason)
    }

    fn get_binding(&self, id: &ResourceId) -> Result<BindingView, PapError> {
        if self
            .failure_write_mode
            .load(std::sync::atomic::Ordering::SeqCst)
            == 4
        {
            return Err(PapError::Persistence);
        }
        self.inner.get_binding(id)
    }

    fn list_bindings(&self, limit: u32, offset: u32) -> Result<Page<BindingView>, PapError> {
        self.inner.list_bindings(limit, offset)
    }
}

type Service = PapService<FakeRepository>;

fn service() -> (Service, Arc<FakeRepository>) {
    let repository = Arc::new(FakeRepository::default());
    (PapService::new(Arc::clone(&repository)), repository)
}

fn policy_template(path: &str) -> PolicySource {
    file_policy(vec![path.to_owned()])
}

#[test]
fn policy_crud_keeps_only_the_current_record_and_never_reuses_revisions() {
    let (pap, repository) = service();
    let first = pap
        .create_policy("protect files", &policy_template("/workspace/a"))
        .unwrap();
    assert_eq!(first.revision.get(), 1);
    assert_eq!(
        pap.update_policy(
            &first.policy_id,
            "protect files",
            &policy_template("/workspace/a")
        )
        .unwrap(),
        first
    );

    let second = pap
        .update_policy(
            &first.policy_id,
            "protect more files",
            &policy_template("/workspace/b"),
        )
        .unwrap();
    assert_eq!(second.revision.get(), 2);
    assert_eq!(
        pap.list_policies(100, 0).unwrap().items,
        vec![second.clone()]
    );
    assert_eq!(
        pap.get_policy(&first.policy_id, first.revision),
        Err(PapError::NotFound)
    );
    assert_eq!(
        pap.delete_policy_revision(&first.policy_id, second.revision)
            .unwrap(),
        second
    );
    assert_eq!(pap.list_policies(100, 0).unwrap().total, 0);

    let third = pap
        .update_policy(
            &first.policy_id,
            "protect newest files",
            &policy_template("/workspace/c"),
        )
        .unwrap();
    assert_eq!(third.revision.get(), 3);
    assert_eq!(
        pap.get_policy(&first.policy_id, second.revision),
        Err(PapError::NotFound)
    );
    assert_eq!(pap.list_policies(100, 0).unwrap().items, vec![third]);
    assert_eq!(repository.list_policies(100, 0).unwrap().total, 1);
    assert_eq!(
        repository
            .get_policy_revision_state(&first.policy_id)
            .unwrap()
            .unwrap()
            .last_allocated_revision
            .get(),
        3
    );

    let missing = ResourceId::new("missing-policy").unwrap();
    assert_eq!(
        pap.update_policy(&missing, "missing", &policy_template("/workspace/missing")),
        Err(PapError::NotFound)
    );
}

#[test]
fn revision_exhaustion_and_pagination_bounds_are_explicit() {
    let (pap, repository) = service();
    let first = pap
        .create_policy("protect files", &policy_template("/workspace/a"))
        .unwrap();
    let maximum = Revision::new(u32::MAX).unwrap();
    let mut exhausted = first.clone();
    exhausted.revision = maximum;
    *repository.policy_read_override.lock().unwrap() = Some(PolicyRevisionState {
        last_allocated_revision: maximum,
        current: Some(exhausted),
    });

    assert_eq!(
        pap.update_policy(
            &first.policy_id,
            "changed",
            &policy_template("/workspace/b"),
        ),
        Err(PapError::RevisionExhausted)
    );
    assert_eq!(pap.list_policies(0, 0), Err(PapError::InvalidPagination));
    assert_eq!(
        pap.list_policies(1_001, 0),
        Err(PapError::InvalidPagination)
    );
}

#[test]
fn scheduling_failure_write_error_does_not_claim_terminal_state() {
    struct Reject;
    impl asc_pap::BindingReconcileEnqueuer for Reject {
        fn check_ready(&self) -> Result<(), PapError> {
            Ok(())
        }
        fn enqueue(&self, _: &ResourceId) -> Result<(), asc_pap::EnqueueError> {
            Err(asc_pap::EnqueueError::Full)
        }
    }
    // Failed write, unchanged Pending after conflict, and failed conflict reread.
    for mode in [1, 2, 3] {
        let (pap, repo) = service();
        repo.failure_write_mode
            .store(mode, std::sync::atomic::Ordering::SeqCst);
        let pap = pap
            .with_reconcile_enqueuer(Arc::new(Reject))
            .with_scope_discovery(Arc::new(Discovery));
        let policy = pap
            .create_policy("test", &policy_template("/workspace/a"))
            .unwrap();
        let scope = pap
            .create_scope_assignment(
                &ScopeSelector::Pid { pid: 4242 },
                &[asc_policy_types::scope::PolicyReference {
                    policy_id: policy.policy_id.clone(),
                    policy_revision: policy.revision,
                }],
                None,
            )
            .unwrap();
        let process = serde_json::from_str::<PreparedBinding>(COMPLETE_BINDING)
            .unwrap()
            .scope
            .process;
        let error = asc_pap::ScopeBindingSink::sync_instances(&pap, &scope.scope_id, &[process])
            .unwrap_err();
        let PapError::SchedulingRejected { id, .. } = error else {
            panic!("expected scheduling rejection")
        };
        let state = repo.binding_state(&id);
        assert_eq!(state.binding.status.phase, BindingStatus::PendingApply);
        assert_eq!(state.binding.status.error, None);
    }
}

fn credential_template() -> PolicySource {
    PolicySource::SecCore(
        serde_json::from_value(serde_json::json!({
            "specVersion":"0.1", "rules":[{
                "effect":"allow", "category":"network", "action":"use_credential",
                "target":{"type":"http_request", "credentialId":"github", "scheme":"https",
                    "host":"api.github.com", "path":"/repos/**", "methods":["GET"],
                    "responseRewrite":"buffered"}
            }]
        }))
        .unwrap(),
    )
}

#[derive(Default)]
struct ScopeLifecycle {
    calls: Mutex<Vec<&'static str>>,
    fail: Mutex<Option<&'static str>>,
    fail_scope: Mutex<Option<ResourceId>>,
    seeds: Mutex<Vec<asc_pap::ScopeDiscoverySeed>>,
    repository: Mutex<Option<Arc<FakeRepository>>>,
    revoke_gate: Mutex<Option<BoundaryGate>>,
    revoking: AtomicBool,
}
impl ScopeLifecycle {
    fn call_for_scope(&self, name: &'static str, id: &ResourceId) -> Result<(), PapError> {
        if self
            .fail_scope
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|failed| failed != id)
        {
            self.calls.lock().unwrap().push(name);
            return Ok(());
        }
        self.call(name)
    }

    fn call(&self, name: &'static str) -> Result<(), PapError> {
        if matches!(name, "validate" | "prepare" | "revoke" | "release") {
            assert!(
                !self.revoking.load(Ordering::SeqCst),
                "credential lifecycle overlapped a blocked revocation: {name}"
            );
        }
        self.calls.lock().unwrap().push(name);
        if *self.fail.lock().unwrap() == Some(name) {
            Err(PapError::Unavailable)
        } else {
            Ok(())
        }
    }
}
impl asc_pap::ScopeExecution for ScopeLifecycle {
    fn validate(&self, scope: &PreparedScope) -> Result<(), PapError> {
        self.call_for_scope("validate", &scope.scope_id)
    }
    fn prepare(&self, scope: &PreparedScope) -> Result<(), PapError> {
        if let Some(repository) = self.repository.lock().unwrap().as_ref() {
            assert_eq!(repository.get_scope(&scope.scope_id).unwrap(), *scope);
        }
        self.call_for_scope("prepare", &scope.scope_id)
    }
    fn revoke(&self, _: &PreparedScope) -> Result<(), PapError> {
        self.call("revoke")?;
        let gate = self.revoke_gate.lock().unwrap().take();
        if let Some(gate) = gate {
            self.revoking.store(true, Ordering::SeqCst);
            gate.wait();
            self.revoking.store(false, Ordering::SeqCst);
        }
        Ok(())
    }
    fn release(&self, _: &ResourceId) -> Result<(), PapError> {
        self.call("release")
    }
}
impl asc_pap::ScopeDiscovery for ScopeLifecycle {
    fn start(&self, seed: &asc_pap::ScopeDiscoverySeed) -> Result<(), PapError> {
        if let Some(repository) = self.repository.lock().unwrap().as_ref() {
            assert_eq!(
                repository.scope_discovery_seed(&seed.scope_id).unwrap(),
                *seed
            );
        }
        self.seeds.lock().unwrap().push(seed.clone());
        self.call_for_scope("start", &seed.scope_id)
    }
    fn stop(&self, _: &ResourceId) -> Result<(), PapError> {
        self.call("stop")
    }
}

fn process_selector() -> ScopeSelector {
    ScopeSelector::Process {
        matcher: asc_policy_types::scope::ProcessMatcher::Name {
            process_name: "agent".into(),
        },
    }
}

fn references(policy: &PreparedPolicy) -> Vec<asc_policy_types::scope::PolicyReference> {
    vec![asc_policy_types::scope::PolicyReference {
        policy_id: policy.policy_id.clone(),
        policy_revision: policy.revision,
    }]
}

#[derive(Clone, Copy, Debug)]
enum ScopeWork {
    Delete,
    Retry,
    Recover,
    Sweep,
}

impl ScopeWork {
    fn run(self, pap: &Service, id: &ResourceId) -> Result<(), PapError> {
        match self {
            Self::Delete => pap
                .delete_scope(id)
                .map(|receipt| assert!(receipt.completed)),
            Self::Retry => pap.retry_scope(id).map(|_| ()),
            Self::Recover => pap.recover_scopes(),
            Self::Sweep => pap.sweep_deleting_scopes(),
        }
    }
}

fn observed_task<T: Send + 'static>(
    threads: &mut Vec<JoinHandle<()>>,
    work: impl FnOnce() -> T + Send + 'static,
) -> (mpsc::Receiver<()>, mpsc::Receiver<T>) {
    let (starting, started) = mpsc::channel();
    let (done, completed) = mpsc::channel();
    threads.push(spawn(move || {
        let _ = starting.send(());
        let _ = done.send(work());
    }));
    (started, completed)
}

fn with_released_gate<T>(
    resume: &mpsc::Sender<()>,
    observe: impl FnOnce(&mut Vec<JoinHandle<()>>) -> T,
) -> T {
    let mut threads = Vec::new();
    let result = catch_unwind(AssertUnwindSafe(|| observe(&mut threads)));
    // Even a timeout/assertion must release the owner before joining every contender.
    let _ = resume.send(());
    let joined: Vec<_> = threads.into_iter().map(JoinHandle::join).collect();
    let result = result.unwrap_or_else(|panic| resume_unwind(panic));
    for thread in joined {
        thread.unwrap();
    }
    result
}

fn credential_scope_service() -> (
    Service,
    Arc<FakeRepository>,
    Arc<ScopeLifecycle>,
    PreparedPolicy,
) {
    let (pap, repo) = service();
    let lifecycle = Arc::new(ScopeLifecycle::default());
    *lifecycle.repository.lock().unwrap() = Some(repo.clone());
    let pap = pap
        .with_scope_execution(lifecycle.clone())
        .with_scope_discovery(lifecycle.clone());
    let policy = pap
        .create_policy("credential", &credential_template())
        .unwrap();
    (pap, repo, lifecycle, policy)
}

#[test]
fn blocked_credential_revocation_allows_file_work_but_serializes_credential_clones() {
    for work in [
        ScopeWork::Delete,
        ScopeWork::Retry,
        ScopeWork::Recover,
        ScopeWork::Sweep,
    ] {
        for mixed in [false, true] {
            let (pap, repo, lifecycle, credential) = credential_scope_service();
            let file = pap
                .create_policy("file", &policy_template("/file"))
                .unwrap();
            let mut refs = references(&credential);
            if mixed {
                refs.extend(references(&file));
            }
            let scope = pap
                .create_scope_assignment(&process_selector(), &refs, Some(1000))
                .unwrap();
            if !matches!(work, ScopeWork::Delete) {
                repo.begin_scope_delete(&scope.scope_id).unwrap();
            }
            let (gate, entered, resume) = BoundaryGate::new();
            *lifecycle.revoke_gate.lock().unwrap() = Some(gate);
            let (blocked, retry, admission) = with_released_gate(&resume, |threads| {
                let blocked_pap = pap.clone();
                let id = scope.scope_id.clone();
                let (_, blocked) = observed_task(threads, move || work.run(&blocked_pap, &id));
                entered
                    .recv_timeout(WAIT)
                    .expect("revocation did not reach gateway");

                let retry_pap = pap.clone();
                let id = scope.scope_id.clone();
                let (retry_started, retry) =
                    observed_task(threads, move || retry_pap.retry_scope(&id).map(|_| ()));
                let admission_pap = pap.clone();
                let (admission_started, admission) = observed_task(threads, move || {
                    admission_pap.create_scope_assignment(&process_selector(), &refs, Some(1000))
                });
                retry_started.recv_timeout(WAIT).unwrap();
                admission_started.recv_timeout(WAIT).unwrap();

                let file_pap = pap.clone();
                let (_, file_done) = observed_task(threads, move || {
                    let created = assignment(&file_pap, &file);
                    ScopeWork::Delete.run(&file_pap, &created.scope_id)
                });
                assert_eq!(
                    file_done
                        .recv_timeout(WAIT)
                        .expect("file work blocked behind gateway"),
                    Ok(()),
                    "{work:?}, mixed={mixed}"
                );
                assert!(lifecycle.revoking.load(Ordering::SeqCst));
                // Contenders were already running; give an unserialized one time to finish.
                for pending in [
                    retry.recv_timeout(SETTLE).map(drop),
                    admission.recv_timeout(SETTLE).map(drop),
                ] {
                    assert_eq!(pending, Err(mpsc::RecvTimeoutError::Timeout));
                }
                (blocked, retry, admission)
            });
            assert_eq!(blocked.recv_timeout(WAIT).unwrap(), Ok(()));
            assert_eq!(retry.recv_timeout(WAIT).unwrap(), Err(PapError::NotFound));
            let admitted = admission.recv_timeout(WAIT).unwrap().unwrap();
            assert_eq!(pap.get_scope(&admitted.scope_id).unwrap(), admitted);
            assert_eq!(pap.get_scope(&scope.scope_id), Err(PapError::NotFound));
        }
    }
}

#[test]
fn stale_scope_reads_cannot_restore_deleted_credential_authority() {
    for work in [
        ScopeWork::Delete,
        ScopeWork::Retry,
        ScopeWork::Recover,
        ScopeWork::Sweep,
    ] {
        for retain_intent in [false, true] {
            let (pap, repo, lifecycle, policy) = credential_scope_service();
            let scope = pap
                .create_scope_assignment(&process_selector(), &references(&policy), Some(1000))
                .unwrap();
            let (gate, captured, resume) = BoundaryGate::new();
            if matches!(work, ScopeWork::Recover | ScopeWork::Sweep) {
                *repo.scope_scan_gate.lock().unwrap() = Some(gate);
            } else {
                *repo.scope_read_gate.lock().unwrap() = Some(gate);
            }
            let stale = with_released_gate(&resume, |threads| {
                let stale_pap = pap.clone();
                let id = scope.scope_id.clone();
                let (_, stale) = observed_task(threads, move || work.run(&stale_pap, &id));
                captured
                    .recv_timeout(WAIT)
                    .expect("Active snapshot was not captured");
                *lifecycle.fail.lock().unwrap() = retain_intent.then_some("revoke");
                let deleting_pap = pap.clone();
                let id = scope.scope_id.clone();
                let (_, deleted) =
                    observed_task(threads, move || ScopeWork::Delete.run(&deleting_pap, &id));
                assert_eq!(
                    deleted
                        .recv_timeout(WAIT)
                        .expect("deletion blocked behind stale read"),
                    if retain_intent {
                        Err(PapError::Unavailable)
                    } else {
                        Ok(())
                    }
                );
                if retain_intent {
                    assert_eq!(
                        pap.get_scope(&scope.scope_id).unwrap().status,
                        asc_policy_types::scope::ScopeStatus::Deleting
                    );
                }
                *lifecycle.fail.lock().unwrap() = None;
                lifecycle.calls.lock().unwrap().clear();
                stale
            });
            let expected = if matches!(work, ScopeWork::Retry) && !retain_intent {
                Err(PapError::NotFound)
            } else {
                Ok(())
            };
            assert_eq!(
                stale.recv_timeout(WAIT).unwrap(),
                expected,
                "{work:?}, retained={retain_intent}"
            );
            let expected_calls = if retain_intent {
                vec!["revoke", "stop", "release"]
            } else {
                vec![]
            };
            assert_eq!(*lifecycle.calls.lock().unwrap(), expected_calls);
            assert_eq!(pap.get_scope(&scope.scope_id), Err(PapError::NotFound));
        }
    }
}

#[test]
fn file_authority_rechecks_credential_revision_after_concurrent_update() {
    let (pap, repo) = service();
    let file = pap
        .create_policy("file", &policy_template("/file"))
        .unwrap();
    let credentials = pap
        .update_policy(&file.policy_id, "credential", &credential_template())
        .unwrap();
    *repo.policy_read_override.lock().unwrap() = Some(PolicyRevisionState {
        current: Some(file.clone()),
        last_allocated_revision: file.revision,
    });
    assert_eq!(
        pap.update_file_policy(&file.policy_id, "downgrade", &file.template),
        Err(PapError::CredentialAuthorityRequired)
    );
    assert_eq!(
        pap.get_policy(&file.policy_id, credentials.revision)
            .unwrap(),
        credentials
    );
}

#[test]
fn credential_admission_validates_before_persistence_and_file_scopes_ignore_execution() {
    struct FileUnavailable;
    impl asc_pap::BindingReconcileEnqueuer for FileUnavailable {
        fn check_ready(&self) -> Result<(), PapError> {
            Err(PapError::Unavailable)
        }
        fn enqueue(&self, _: &ResourceId) -> Result<(), asc_pap::EnqueueError> {
            Ok(())
        }
    }
    let (pap, repo) = service();
    let lifecycle = Arc::new(ScopeLifecycle::default());
    *lifecycle.repository.lock().unwrap() = Some(repo.clone());
    let pap = pap.with_scope_discovery(lifecycle.clone());
    let policy = pap
        .create_policy("credential", &credential_template())
        .unwrap();
    let refs = references(&policy);
    for (selector, owner) in [
        (process_selector(), None),
        (process_selector(), Some(0)),
        (ScopeSelector::Pid { pid: 1 }, Some(1000)),
    ] {
        assert!(matches!(
            pap.create_scope_assignment(&selector, &refs, owner),
            Err(PapError::InvalidScope(_))
        ));
    }
    assert_eq!(
        pap.create_scope_assignment(&process_selector(), &refs, Some(1000)),
        Err(PapError::Unavailable)
    );
    let pap = pap.with_scope_execution(lifecycle.clone());
    *lifecycle.fail.lock().unwrap() = Some("validate");
    assert_eq!(
        pap.create_scope_assignment(&process_selector(), &refs, Some(1000)),
        Err(PapError::Unavailable)
    );
    assert_eq!(repo.list_scopes(10, 0).unwrap().total, 0);
    *lifecycle.fail.lock().unwrap() = None;
    lifecycle.calls.lock().unwrap().clear();
    let unavailable_file = pap
        .clone()
        .with_reconcile_enqueuer(Arc::new(FileUnavailable));
    let scope = unavailable_file
        .create_scope_assignment(&process_selector(), &refs, Some(1000))
        .unwrap();
    assert_eq!(scope.owner_uid, Some(1000));
    assert_eq!(
        *lifecycle.calls.lock().unwrap(),
        ["validate", "prepare", "start"]
    );
    assert!(pap.delete_scope(&scope.scope_id).unwrap().completed);
    let file = pap
        .create_policy("file", &policy_template("/file"))
        .unwrap();
    assert_eq!(
        unavailable_file.create_scope_assignment(&process_selector(), &references(&file), None),
        Err(PapError::Unavailable)
    );
    lifecycle.calls.lock().unwrap().clear();
    *lifecycle.fail.lock().unwrap() = Some("validate");
    let scope = pap
        .create_scope_assignment(&process_selector(), &references(&file), None)
        .unwrap();
    assert!(pap.delete_scope(&scope.scope_id).unwrap().completed);
    assert_eq!(*lifecycle.calls.lock().unwrap(), ["start", "stop"]);
}

#[test]
fn credential_failures_compensate_in_order_and_retry_retains_zero_instance_responsibility() {
    for failing in [
        "prepare",
        "start",
        "revoke",
        "stop",
        "release",
        "ready_query",
    ] {
        let (pap, repo) = service();
        let lifecycle = Arc::new(ScopeLifecycle::default());
        let pap = pap
            .with_scope_execution(lifecycle.clone())
            .with_scope_discovery(lifecycle.clone());
        let policy = pap
            .create_policy("credential", &credential_template())
            .unwrap();
        if matches!(failing, "prepare" | "start") {
            *lifecycle.fail.lock().unwrap() = Some(failing);
            assert_eq!(
                pap.create_scope_assignment(&process_selector(), &references(&policy), Some(1000)),
                Err(PapError::Unavailable)
            );
            assert_eq!(repo.list_scopes(10, 0).unwrap().total, 0);
            let expected = if failing == "prepare" {
                vec!["validate", "prepare", "revoke", "stop", "release"]
            } else {
                vec!["validate", "prepare", "start", "revoke", "stop", "release"]
            };
            assert_eq!(*lifecycle.calls.lock().unwrap(), expected);
        } else {
            let scope = pap
                .create_scope_assignment(&process_selector(), &references(&policy), Some(1000))
                .unwrap();
            lifecycle.calls.lock().unwrap().clear();
            *lifecycle.fail.lock().unwrap() = Some(failing);
            let expected_error = if failing == "ready_query" {
                repo.scope_cleanup_failure
                    .store(3, std::sync::atomic::Ordering::SeqCst);
                PapError::Persistence
            } else {
                PapError::Unavailable
            };
            assert_eq!(pap.delete_scope(&scope.scope_id), Err(expected_error));
            assert_eq!(
                repo.get_scope(&scope.scope_id).unwrap().status,
                asc_policy_types::scope::ScopeStatus::Deleting
            );
            assert_eq!(repo.list_bindings(10, 0).unwrap().total, 0);
            let expected = match failing {
                "revoke" | "ready_query" => vec!["revoke"],
                "stop" => vec!["revoke", "stop"],
                "release" => vec!["revoke", "stop", "release"],
                _ => unreachable!(),
            };
            assert_eq!(*lifecycle.calls.lock().unwrap(), expected);
            *lifecycle.fail.lock().unwrap() = None;
            repo.scope_cleanup_failure
                .store(0, std::sync::atomic::Ordering::SeqCst);
            pap.retry_scope(&scope.scope_id).unwrap();
            assert_eq!(repo.get_scope(&scope.scope_id), Err(PapError::NotFound));
        }
    }
}

#[test]
fn failed_start_and_revocation_keep_zero_binding_scope_for_durable_sweep() {
    struct FailingStart(Arc<ScopeLifecycle>);
    impl asc_pap::ScopeDiscovery for FailingStart {
        fn start(&self, _: &asc_pap::ScopeDiscoverySeed) -> Result<(), PapError> {
            self.0.call("start")?;
            Err(PapError::Unavailable)
        }
        fn stop(&self, _: &ResourceId) -> Result<(), PapError> {
            self.0.call("stop")
        }
    }
    let (pap, repo) = service();
    let lifecycle = Arc::new(ScopeLifecycle::default());
    *lifecycle.fail.lock().unwrap() = Some("revoke");
    let pap = pap
        .with_scope_execution(lifecycle.clone())
        .with_scope_discovery(Arc::new(FailingStart(lifecycle.clone())));
    let policy = pap
        .create_policy("credential", &credential_template())
        .unwrap();
    assert_eq!(
        pap.create_scope_assignment(&process_selector(), &references(&policy), Some(1000)),
        Err(PapError::Unavailable)
    );
    let scopes = repo.list_scopes(10, 0).unwrap();
    assert_eq!(scopes.total, 1);
    let scope = &scopes.items[0];
    assert_eq!(scope.status, asc_policy_types::scope::ScopeStatus::Deleting);
    assert_eq!(repo.list_bindings(10, 0).unwrap().total, 0);
    assert!(
        !repo
            .scope_execution_ready_to_release(&scope.scope_id)
            .unwrap()
    );
    assert_eq!(
        *lifecycle.calls.lock().unwrap(),
        ["validate", "prepare", "start", "revoke"]
    );
    *lifecycle.fail.lock().unwrap() = None;
    let pap = pap.with_scope_discovery(lifecycle);
    pap.sweep_deleting_scopes().unwrap();
    assert_eq!(repo.get_scope(&scope.scope_id), Err(PapError::NotFound));
}

#[test]
fn recovery_prepares_pinned_active_scope_and_resumes_failed_deletion() {
    let (pap, repo) = service();
    let lifecycle = Arc::new(ScopeLifecycle::default());
    let pap = pap
        .with_scope_execution(lifecycle.clone())
        .with_scope_discovery(lifecycle.clone());
    let policy = pap
        .create_policy("credential", &credential_template())
        .unwrap();
    let scope = pap
        .create_scope_assignment(&process_selector(), &references(&policy), Some(1000))
        .unwrap();
    pap.delete_policy_revision(&policy.policy_id, policy.revision)
        .unwrap();
    let recovered = PapService::new(repo.clone())
        .with_scope_execution(lifecycle.clone())
        .with_scope_discovery(lifecycle.clone());
    lifecycle.calls.lock().unwrap().clear();
    recovered.recover_scopes().unwrap();
    assert_eq!(
        *lifecycle.calls.lock().unwrap(),
        ["validate", "prepare", "start"]
    );
    assert_eq!(recovered.get_scope(&scope.scope_id).unwrap(), scope);
    *lifecycle.fail.lock().unwrap() = Some("revoke");
    assert_eq!(
        recovered.delete_scope(&scope.scope_id),
        Err(PapError::Unavailable)
    );
    *lifecycle.fail.lock().unwrap() = None;
    lifecycle.calls.lock().unwrap().clear();
    let recovered = PapService::new(repo)
        .with_scope_execution(lifecycle.clone())
        .with_scope_discovery(lifecycle.clone());
    recovered.recover_scopes().unwrap();
    assert_eq!(
        *lifecycle.calls.lock().unwrap(),
        ["revoke", "stop", "release"]
    );
    assert_eq!(
        recovered.get_scope(&scope.scope_id),
        Err(PapError::NotFound)
    );
}

#[test]
fn active_recovery_failure_preserves_authority_and_explicit_retry_restores_before_bindings() {
    use asc_pap::ScopeBindingSink;
    for failing in ["validate", "prepare", "start"] {
        let (pap, repo) = service();
        let lifecycle = Arc::new(ScopeLifecycle::default());
        *lifecycle.repository.lock().unwrap() = Some(repo.clone());
        let pap = pap
            .with_scope_execution(lifecycle.clone())
            .with_scope_discovery(lifecycle.clone());
        let policy = pap
            .create_policy("credential", &credential_template())
            .unwrap();
        for _ in 0..2 {
            pap.create_scope_assignment(&process_selector(), &references(&policy), Some(1000))
                .unwrap();
        }
        let scopes = pap.list_scopes(10, 0).unwrap().items;
        let scope = &scopes[0];
        let healthy = &scopes[1];
        let instance = serde_json::from_str::<PreparedBinding>(COMPLETE_BINDING)
            .unwrap()
            .scope
            .process;
        pap.sync_instances(&scope.scope_id, &[instance]).unwrap();
        let binding = pap.list_bindings(10, 0).unwrap().items.remove(0);
        repo.transition(&binding.spec.binding_id, BindingStatus::Applying);
        repo.transition(&binding.spec.binding_id, BindingStatus::ApplyFailed);
        let failed_binding = repo.binding_state(&binding.spec.binding_id);
        let seed = repo.scope_discovery_seed(&scope.scope_id).unwrap();
        pap.delete_policy_revision(&policy.policy_id, policy.revision)
            .unwrap();
        let recovered = PapService::new(repo.clone())
            .with_scope_execution(lifecycle.clone())
            .with_scope_discovery(lifecycle.clone());
        *lifecycle.fail.lock().unwrap() = Some(failing);
        *lifecycle.fail_scope.lock().unwrap() = Some(scope.scope_id.clone());
        lifecycle.calls.lock().unwrap().clear();
        lifecycle.seeds.lock().unwrap().clear();
        assert_eq!(recovered.recover_scopes(), Ok(()));
        assert_eq!(recovered.list_scopes(10, 0).unwrap().items, scopes);
        assert_eq!(repo.binding_state(&binding.spec.binding_id), failed_binding);
        let failed_calls = match failing {
            "validate" => vec!["validate"],
            "prepare" => vec!["validate", "prepare"],
            "start" => vec!["validate", "prepare", "start"],
            _ => unreachable!(),
        };
        let mut expected = failed_calls.clone();
        expected.extend(["validate", "prepare", "start"]);
        assert_eq!(*lifecycle.calls.lock().unwrap(), expected);
        assert_eq!(
            lifecycle.seeds.lock().unwrap().last(),
            Some(&repo.scope_discovery_seed(&healthy.scope_id).unwrap())
        );

        lifecycle.calls.lock().unwrap().clear();
        assert_eq!(
            recovered.retry_scope(&scope.scope_id),
            Err(PapError::Unavailable)
        );
        assert_eq!(*lifecycle.calls.lock().unwrap(), failed_calls);
        assert_eq!(repo.binding_state(&binding.spec.binding_id), failed_binding);
        assert_eq!(recovered.get_scope(&scope.scope_id).unwrap(), *scope);

        *lifecycle.fail.lock().unwrap() = None;
        lifecycle.calls.lock().unwrap().clear();
        lifecycle.seeds.lock().unwrap().clear();
        assert_eq!(recovered.retry_scope(&scope.scope_id).unwrap(), *scope);
        assert_eq!(
            *lifecycle.calls.lock().unwrap(),
            ["validate", "prepare", "start"]
        );
        assert_eq!(*lifecycle.seeds.lock().unwrap(), vec![seed]);
        let retried = recovered.get_binding(&binding.spec.binding_id).unwrap();
        assert_eq!(retried.spec, binding.spec);
        assert_eq!(retried.status, BindingStatus::PendingApply);
    }
}

#[test]
fn file_recovery_reports_discovery_failure_without_changing_file_retry_behavior() {
    use asc_pap::ScopeBindingSink;
    let (pap, repo) = service();
    let lifecycle = Arc::new(ScopeLifecycle::default());
    let pap = pap.with_scope_discovery(lifecycle.clone());
    let policy = pap
        .create_policy("file", &policy_template("/file"))
        .unwrap();
    let scope = assignment(&pap, &policy);
    let instance = serde_json::from_str::<PreparedBinding>(COMPLETE_BINDING)
        .unwrap()
        .scope
        .process;
    pap.sync_instances(&scope.scope_id, &[instance]).unwrap();
    let binding = pap.list_bindings(10, 0).unwrap().items.remove(0);
    repo.transition(&binding.spec.binding_id, BindingStatus::Applying);
    repo.transition(&binding.spec.binding_id, BindingStatus::ApplyFailed);
    let failed = repo.binding_state(&binding.spec.binding_id);
    *lifecycle.fail.lock().unwrap() = Some("start");
    let recovered = PapService::new(repo.clone()).with_scope_discovery(lifecycle.clone());
    assert_eq!(recovered.recover_scopes(), Err(PapError::Unavailable));
    assert_eq!(recovered.get_scope(&scope.scope_id).unwrap(), scope);
    assert_eq!(repo.binding_state(&binding.spec.binding_id), failed);
    lifecycle.calls.lock().unwrap().clear();
    assert_eq!(recovered.retry_scope(&scope.scope_id).unwrap(), scope);
    assert!(lifecycle.calls.lock().unwrap().is_empty());
    let retried = recovered.get_binding(&binding.spec.binding_id).unwrap();
    assert_eq!(retried.spec, binding.spec);
    assert_eq!(retried.status, BindingStatus::PendingApply);
}

#[test]
fn deleting_credential_scope_survives_last_binding_cleanup_until_durable_execution_release() {
    use asc_pap::ScopeBindingSink;
    let (pap, repo) = service();
    let lifecycle = Arc::new(ScopeLifecycle::default());
    let pap = pap
        .with_scope_execution(lifecycle.clone())
        .with_scope_discovery(lifecycle.clone());
    let policy = pap
        .create_policy("credential", &credential_template())
        .unwrap();
    let scope = pap
        .create_scope_assignment(&process_selector(), &references(&policy), Some(1000))
        .unwrap();
    let instance = serde_json::from_str::<PreparedBinding>(COMPLETE_BINDING)
        .unwrap()
        .scope
        .process;
    pap.sync_instances(&scope.scope_id, &[instance]).unwrap();
    let binding = pap.list_bindings(10, 0).unwrap().items.remove(0);
    assert!(!pap.delete_scope(&scope.scope_id).unwrap().completed);
    assert!(!lifecycle.calls.lock().unwrap().contains(&"release"));
    repo.transition(&binding.spec.binding_id, BindingStatus::Deleting);
    let deleting = repo.binding_state(&binding.spec.binding_id);
    repo.inner
        .compare_exchange_binding_state(&deleting, &BindingStateWrite::delete())
        .unwrap();
    assert_eq!(
        repo.get_scope(&scope.scope_id).unwrap().status,
        asc_policy_types::scope::ScopeStatus::Deleting
    );
    assert!(
        repo.scope_execution_ready_to_release(&scope.scope_id)
            .unwrap()
    );
    lifecycle.calls.lock().unwrap().clear();
    *lifecycle.fail.lock().unwrap() = Some("release");
    assert_eq!(pap.sweep_deleting_scopes(), Err(PapError::Unavailable));
    assert!(repo.get_scope(&scope.scope_id).is_ok());
    assert_eq!(*lifecycle.calls.lock().unwrap(), ["release"]);
    *lifecycle.fail.lock().unwrap() = None;
    pap.sweep_deleting_scopes().unwrap();
    assert_eq!(*lifecycle.calls.lock().unwrap(), ["release", "release"]);
    assert_eq!(repo.get_scope(&scope.scope_id), Err(PapError::NotFound));
}

struct Discovery;
impl asc_pap::ScopeDiscovery for Discovery {
    fn start(&self, _: &asc_pap::ScopeDiscoverySeed) -> Result<(), PapError> {
        Ok(())
    }
    fn stop(&self, _: &ResourceId) -> Result<(), PapError> {
        Ok(())
    }
}

fn assignment(pap: &Service, policy: &PreparedPolicy) -> PreparedScope {
    pap.create_scope_assignment(
        &ScopeSelector::Pid { pid: 4242 },
        &[asc_policy_types::scope::PolicyReference {
            policy_id: policy.policy_id.clone(),
            policy_revision: policy.revision,
        }],
        None,
    )
    .unwrap()
}

#[test]
fn assignment_snapshots_survive_policy_update_delete_and_later_discovery() {
    use asc_pap::ScopeBindingSink;
    let (pap, repo) = service();
    let pap = pap.with_scope_discovery(Arc::new(Discovery));
    let original = pap.create_policy("old", &policy_template("/a")).unwrap();
    let scope = assignment(&pap, &original);
    let updated = pap
        .update_policy(&original.policy_id, "new", &policy_template("/b"))
        .unwrap();
    assert_eq!(updated.revision.get(), 2);
    assert!(matches!(
        pap.create_scope_assignment(
            &scope.selector,
            &[asc_policy_types::scope::PolicyReference {
                policy_id: original.policy_id.clone(),
                policy_revision: original.revision
            }],
            None,
        ),
        Err(PapError::ReferencedPolicyRevisionNotFound)
    ));
    pap.delete_policy_revision(&updated.policy_id, updated.revision)
        .unwrap();
    assert_eq!(pap.get_scope(&scope.scope_id).unwrap(), scope);
    let instance = serde_json::from_str::<PreparedBinding>(COMPLETE_BINDING)
        .unwrap()
        .scope
        .process;
    pap.sync_instances(&scope.scope_id, std::slice::from_ref(&instance))
        .unwrap();
    pap.sync_instances(&scope.scope_id, std::slice::from_ref(&instance))
        .unwrap();
    let bindings = pap.list_bindings(10, 0).unwrap();
    assert_eq!(bindings.total, 1);
    assert_eq!(bindings.items[0].spec.policy, original);
    assert_eq!(bindings.items[0].spec.scope.process, instance);
    let wire = serde_json::to_value(&scope).unwrap();
    assert!(wire.get("revision").is_none());
    assert!(wire.get("ownerUid").is_none());
    assert!(wire.get("policyTemplates").is_none());
    assert_eq!(wire["policySnapshots"], serde_json::json!([original]));
    assert_eq!(repo.list_policies(10, 0).unwrap().total, 0);
}

#[test]
fn uncertain_scope_commit_requires_exact_saved_assignment_before_discovery() {
    #[derive(Default)]
    struct RecordingDiscovery(Mutex<Vec<PreparedScope>>);
    impl asc_pap::ScopeDiscovery for RecordingDiscovery {
        fn start(&self, seed: &asc_pap::ScopeDiscoverySeed) -> Result<(), PapError> {
            self.0.lock().unwrap().push(seed.scope.clone());
            Ok(())
        }
        fn stop(&self, _: &ResourceId) -> Result<(), PapError> {
            Ok(())
        }
    }
    for mode in [1, 2] {
        let (pap, repo) = service();
        let discovery = Arc::new(RecordingDiscovery::default());
        let pap = pap.with_scope_discovery(discovery.clone());
        let policy = pap.create_policy("policy", &policy_template("/a")).unwrap();
        repo.scope_commit_failure
            .store(mode, std::sync::atomic::Ordering::SeqCst);
        let result = pap.create_scope_assignment(
            &ScopeSelector::Pid { pid: 4242 },
            &[asc_policy_types::scope::PolicyReference {
                policy_id: policy.policy_id.clone(),
                policy_revision: policy.revision,
            }],
            None,
        );
        if mode == 1 {
            assert_eq!(result, Err(PapError::Persistence));
            assert!(discovery.0.lock().unwrap().is_empty());
            assert_eq!(repo.list_scopes(10, 0).unwrap().total, 0);
        } else {
            let saved = result.unwrap();
            assert_eq!(*discovery.0.lock().unwrap(), vec![saved.clone()]);
            assert_eq!(repo.get_scope(&saved.scope_id).unwrap(), saved);
            assert_eq!(repo.list_scopes(10, 0).unwrap().total, 1);
        }
    }
}

#[test]
fn snapshot_admission_races_policy_mutation_atomically() {
    use std::sync::Barrier;
    for delete in [false, true] {
        for iteration in 0..32 {
            let (pap, repo) = service();
            let original = pap.create_policy("old", &policy_template("/a")).unwrap();
            let candidate = PreparedScope {
                scope_id: ResourceId::new(format!("scope-{iteration}")).unwrap(),
                selector: ScopeSelector::Pid { pid: 4242 },
                policy_snapshots: vec![original.clone()],
                owner_uid: None,
                status: asc_policy_types::scope::ScopeStatus::Active,
            };
            let gate = Arc::new(Barrier::new(2));
            let admission = {
                let gate = gate.clone();
                let repo = repo.clone();
                let candidate = candidate.clone();
                std::thread::spawn(move || {
                    gate.wait();
                    repo.put_scope(&candidate)
                })
            };
            gate.wait();
            if delete {
                pap.delete_policy_revision(&original.policy_id, original.revision)
                    .unwrap();
            } else {
                pap.update_policy(&original.policy_id, "new", &original.template)
                    .unwrap();
            }
            match admission.join().unwrap() {
                Ok(saved) => assert_eq!(saved, candidate),
                Err(error) => assert_eq!(error, PapError::ReferencedPolicyRevisionNotFound),
            }
            if let Ok(saved) = pap.get_scope(&candidate.scope_id) {
                assert_eq!(saved.policy_snapshots, vec![original]);
            }
        }
    }
}

#[test]
fn deleting_scope_fences_discovery_and_retains_failures_until_explicit_retry() {
    use asc_pap::ScopeBindingSink;
    let (pap, repo) = service();
    let pap = pap.with_scope_discovery(Arc::new(Discovery));
    let policy = pap.create_policy("policy", &policy_template("/a")).unwrap();
    let scope = assignment(&pap, &policy);
    let other = assignment(&pap, &policy);
    let instance = serde_json::from_str::<PreparedBinding>(COMPLETE_BINDING)
        .unwrap()
        .scope
        .process;
    for scope in [&scope, &other] {
        pap.sync_instances(&scope.scope_id, std::slice::from_ref(&instance))
            .unwrap();
    }
    let binding = pap
        .list_bindings(10, 0)
        .unwrap()
        .items
        .into_iter()
        .find(|b| b.spec.scope.scope_id == scope.scope_id)
        .unwrap();
    repo.transition(&binding.spec.binding_id, BindingStatus::Applying);
    let stale_apply = repo.binding_state(&binding.spec.binding_id);
    pap.delete_scope(&scope.scope_id).unwrap();
    assert_eq!(
        pap.sync_instances(&scope.scope_id, &[instance]),
        Err(PapError::OperationInProgress)
    );
    let mut late_ready = stale_apply.clone();
    late_ready.binding.status = BindingStatus::Ready.into();
    assert_eq!(
        repo.inner
            .compare_exchange_binding_state(&stale_apply, &BindingStateWrite::new(late_ready)),
        Ok(WriteResult::Conflict)
    );
    repo.transition(&binding.spec.binding_id, BindingStatus::Deleting);
    repo.transition(&binding.spec.binding_id, BindingStatus::DeleteFailed);
    let failed = pap.get_binding(&binding.spec.binding_id).unwrap();
    pap.delete_scope(&scope.scope_id).unwrap();
    assert_eq!(pap.get_binding(&binding.spec.binding_id).unwrap(), failed);
    assert_eq!(pap.get_scope(&other.scope_id).unwrap(), other);
    pap.retry_scope(&scope.scope_id).unwrap();
    assert_eq!(
        pap.get_binding(&binding.spec.binding_id).unwrap().status,
        BindingStatus::PendingDelete
    );
    assert_eq!(
        pap.get_binding(&binding.spec.binding_id).unwrap().spec,
        binding.spec
    );
    assert_eq!(pap.list_bindings(10, 0).unwrap().total, 2);
}

#[test]
fn instance_reuse_retires_old_binding_and_creates_distinct_identity() {
    use asc_pap::ScopeBindingSink;
    let (pap, _) = service();
    let pap = pap.with_scope_discovery(Arc::new(Discovery));
    let policy = pap.create_policy("policy", &policy_template("/a")).unwrap();
    let scope = pap
        .create_scope_assignment(
            &ScopeSelector::Process {
                matcher: asc_policy_types::scope::ProcessMatcher::Name {
                    process_name: "agent".into(),
                },
            },
            &[asc_policy_types::scope::PolicyReference {
                policy_id: policy.policy_id.clone(),
                policy_revision: policy.revision,
            }],
            None,
        )
        .unwrap();
    let mut instance = serde_json::from_str::<PreparedBinding>(COMPLETE_BINDING)
        .unwrap()
        .scope
        .process;
    pap.sync_instances(&scope.scope_id, std::slice::from_ref(&instance))
        .unwrap();
    let first = pap.list_bindings(10, 0).unwrap().items.remove(0);
    instance.start_time += 1;
    pap.sync_instances(&scope.scope_id, std::slice::from_ref(&instance))
        .unwrap();
    assert_eq!(
        pap.get_binding(&first.spec.binding_id).unwrap().status,
        BindingStatus::PendingDelete
    );
    let all = pap.list_bindings(10, 0).unwrap();
    assert_eq!(all.total, 2);
    let second = all
        .items
        .iter()
        .find(|b| b.spec.binding_id != first.spec.binding_id)
        .unwrap();
    assert_eq!(second.spec.scope.process, instance);
    assert_eq!(second.status, BindingStatus::PendingApply);
}

#[test]
fn discovery_start_error_survives_compensation_failures() {
    struct UnavailableDiscovery;
    impl asc_pap::ScopeDiscovery for UnavailableDiscovery {
        fn start(&self, _: &asc_pap::ScopeDiscoverySeed) -> Result<(), PapError> {
            Err(PapError::Unavailable)
        }
        fn stop(&self, _: &ResourceId) -> Result<(), PapError> {
            panic!("failed start must not leave a worker to stop")
        }
    }
    for failure in 0..=2 {
        let (pap, repo) = service();
        repo.scope_cleanup_failure
            .store(failure, std::sync::atomic::Ordering::SeqCst);
        let pap = pap.with_scope_discovery(Arc::new(UnavailableDiscovery));
        let policy = pap
            .create_policy("test", &policy_template("/protected"))
            .unwrap();
        assert_eq!(
            pap.create_scope_assignment(
                &ScopeSelector::Pid { pid: 4242 },
                &[asc_policy_types::scope::PolicyReference {
                    policy_id: policy.policy_id.clone(),
                    policy_revision: policy.revision,
                }],
                None,
            ),
            Err(PapError::Unavailable),
            "compensation failure at step {failure} must not replace the startup error"
        );
        let remaining = pap.list_scopes(10, 0).unwrap();
        if failure == 0 {
            assert_eq!(remaining.total, 0);
        } else {
            assert_eq!(remaining.total, 1);
            let scope = &remaining.items[0];
            assert_eq!(scope.policy_snapshots, vec![policy]);
            assert_eq!(
                scope.status,
                if failure == 1 {
                    asc_policy_types::scope::ScopeStatus::Active
                } else {
                    asc_policy_types::scope::ScopeStatus::Deleting
                }
            );
            repo.scope_cleanup_failure
                .store(0, std::sync::atomic::Ordering::SeqCst);
            // After the storage failure clears, explicit deletion can finish cleanup.
            let pap = pap.with_scope_discovery(Arc::new(Discovery));
            assert!(pap.delete_scope(&scope.scope_id).unwrap().completed);
        }
    }
}
