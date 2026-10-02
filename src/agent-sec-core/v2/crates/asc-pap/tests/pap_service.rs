use std::sync::{Arc, Barrier, Mutex};

use asc_foundation_types::{ResourceId, Revision};
use asc_pap::{
    Page, PapError, PapRepository, PapService, PolicyCompiler, PolicyRevisionState,
    ScopeRevisionState,
};
use asc_pap_repository_memory::ProcessLocalPapRepository;
use asc_policy_repository::{
    BindingStateRepository, BindingStateSnapshot, BindingStateWrite, WriteResult,
};
use asc_policy_types::authoring::{PolicyTemplate, TemplateEnvelope};
use asc_policy_types::binding::{BindingStatus, BindingView, PreparedBinding};
use asc_policy_types::error::ValidationError;
use asc_policy_types::identifiers::PolicyId;
use asc_policy_types::policy::{PolicyEnvelope, PreparedPolicy};
use asc_policy_types::scope::{PreparedScope, ScopeSelector};

const COMPLETE_BINDING: &str =
    include_str!("../../asc-policy-types/tests/fixtures/prepared-binding.json");

#[derive(Default)]
struct FakeRepository {
    inner: ProcessLocalPapRepository,
    failure_write_mode: std::sync::atomic::AtomicUsize,
    scope_read_gate: Mutex<ScopeReadGate>,
    policy_read_override: Mutex<Option<PolicyRevisionState>>,
}

#[derive(Default)]
struct ScopeReadGate {
    barrier: Option<Arc<Barrier>>,
    remaining: usize,
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
        assert_eq!(
            self.inner
                .compare_exchange_binding_state(&current, &BindingStateWrite::new(next)),
            Ok(WriteResult::Applied)
        );
    }

    fn synchronize_next_scope_reads(&self, participants: usize) {
        let mut gate = self.scope_read_gate.lock().unwrap();
        assert!(gate.barrier.is_none());
        gate.barrier = Some(Arc::new(Barrier::new(participants)));
        gate.remaining = participants;
    }

    fn take_scope_read_barrier(&self) -> Result<Option<Arc<Barrier>>, PapError> {
        let mut gate = self
            .scope_read_gate
            .lock()
            .map_err(|_| PapError::Persistence)?;
        let Some(barrier) = gate.barrier.clone() else {
            return Ok(None);
        };
        gate.remaining -= 1;
        if gate.remaining == 0 {
            gate.barrier = None;
        }
        Ok(Some(barrier))
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
        self.inner.put_scope(scope)
    }

    fn get_scope_revision_state(
        &self,
        id: &ResourceId,
    ) -> Result<Option<ScopeRevisionState>, PapError> {
        let result = self.inner.get_scope_revision_state(id)?;
        if let Some(barrier) = self.take_scope_read_barrier()? {
            barrier.wait();
        }
        Ok(result)
    }

    fn get_scope(&self, id: &ResourceId, revision: Revision) -> Result<PreparedScope, PapError> {
        self.inner.get_scope(id, revision)
    }

    fn list_scopes(&self, limit: u32, offset: u32) -> Result<Page<PreparedScope>, PapError> {
        self.inner.list_scopes(limit, offset)
    }

    fn delete_scope_revision(
        &self,
        id: &ResourceId,
        revision: Revision,
    ) -> Result<PreparedScope, PapError> {
        self.inner.delete_scope_revision(id, revision)
    }

    fn update_binding(
        &self,
        expected: Option<&BindingView>,
        binding: &BindingView,
    ) -> Result<BindingView, PapError> {
        self.inner.update_binding(expected, binding)
    }

    fn fail_pending_binding(
        &self,
        expected: &BindingView,
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

struct FixtureCompiler {
    mismatch_identity: bool,
}

impl PolicyCompiler for FixtureCompiler {
    fn lower(&self, template: &TemplateEnvelope) -> Result<PolicyEnvelope, ValidationError> {
        let fixture: PreparedBinding = serde_json::from_str(COMPLETE_BINDING)
            .map_err(|error| ValidationError::new("fixture", error.to_string()))?;
        let mut policy = fixture.policy.canonical_policy;
        policy.policy_id = if self.mismatch_identity {
            PolicyId::new("compiler-mismatch")
                .map_err(|error| ValidationError::new("policyId", error))?
        } else {
            template.policy_id.clone()
        };
        policy.revision = template.revision;
        Ok(policy)
    }
}

type Service = PapService<FakeRepository, FixtureCompiler>;

fn service() -> (Service, Arc<FakeRepository>) {
    let repository = Arc::new(FakeRepository::default());
    let compiler = Arc::new(FixtureCompiler {
        mismatch_identity: false,
    });
    (
        PapService::new(Arc::clone(&repository), compiler),
        repository,
    )
}

fn policy_template(path: &str) -> PolicyTemplate {
    PolicyTemplate::PreventFileDeletion {
        files: vec![path.to_owned()],
    }
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
fn scope_crud_keeps_only_the_current_record_and_preserves_revision_heads() {
    let (pap, repository) = service();
    let first = pap.create_scope(&ScopeSelector::Pid { pid: 4242 }).unwrap();
    assert_eq!(first.revision.get(), 1);
    assert_eq!(
        pap.update_scope(&first.scope_id, &ScopeSelector::Pid { pid: 4242 })
            .unwrap(),
        first
    );

    let second = pap
        .update_scope(&first.scope_id, &ScopeSelector::CgroupId { cgroup_id: 99 })
        .unwrap();
    assert_eq!(second.revision.get(), 2);
    assert_eq!(pap.list_scopes(100, 0).unwrap().items, vec![second.clone()]);
    assert_eq!(
        pap.get_scope(&first.scope_id, first.revision),
        Err(PapError::NotFound)
    );
    pap.delete_scope_revision(&first.scope_id, second.revision)
        .unwrap();
    assert_eq!(pap.list_scopes(100, 0).unwrap().total, 0);
    let third = pap
        .update_scope(&first.scope_id, &ScopeSelector::Pid { pid: 7 })
        .unwrap();
    assert_eq!(third.revision.get(), 3);
    assert_eq!(pap.list_scopes(100, 0).unwrap().items, vec![third]);
    assert_eq!(repository.list_scopes(100, 0).unwrap().total, 1);
    assert_eq!(
        repository
            .get_scope_revision_state(&first.scope_id)
            .unwrap()
            .unwrap()
            .last_allocated_revision
            .get(),
        3
    );

    assert!(matches!(
        pap.create_scope(&ScopeSelector::Pid { pid: 0 }),
        Err(PapError::InvalidScope(_))
    ));
    let missing = ResourceId::new("missing-scope").unwrap();
    assert_eq!(
        pap.update_scope(&missing, &ScopeSelector::Pid { pid: 9 }),
        Err(PapError::NotFound)
    );
}

#[test]
fn concurrent_scope_updates_retry_after_repository_cas_conflict() {
    let (pap, repository) = service();
    let first = pap.create_scope(&ScopeSelector::Pid { pid: 4242 }).unwrap();
    repository.synchronize_next_scope_reads(2);

    let left = {
        let pap = pap.clone();
        let scope_id = first.scope_id.clone();
        std::thread::spawn(move || pap.update_scope(&scope_id, &ScopeSelector::Pid { pid: 7 }))
    };
    let right = {
        let pap = pap.clone();
        let scope_id = first.scope_id.clone();
        std::thread::spawn(move || {
            pap.update_scope(&scope_id, &ScopeSelector::CgroupId { cgroup_id: 99 })
        })
    };

    let mut updates = [
        left.join().unwrap().unwrap(),
        right.join().unwrap().unwrap(),
    ];
    updates.sort_by_key(|scope| scope.revision);

    assert_eq!(updates[0].revision.get(), 2);
    assert_eq!(updates[1].revision.get(), 3);
    assert_ne!(updates[0].selector, updates[1].selector);
    assert_eq!(
        pap.get_scope(&first.scope_id, updates[0].revision),
        Err(PapError::NotFound)
    );
    assert_eq!(
        pap.get_scope(&first.scope_id, updates[1].revision).unwrap(),
        updates[1]
    );
}

#[test]
fn binding_revision_tracks_spec_and_delete_intent_is_irreversible() {
    let (pap, repository) = service();
    let policy = pap
        .create_policy("protect files", &policy_template("/workspace/a"))
        .unwrap();
    let scope = pap.create_scope(&ScopeSelector::Pid { pid: 4242 }).unwrap();
    let first = pap
        .create_binding(
            &policy.policy_id,
            policy.revision,
            &scope.scope_id,
            scope.revision,
        )
        .unwrap();
    let id = &first.spec.binding_id;
    let apply = |policy: &PreparedPolicy| {
        pap.update_binding(
            id,
            &policy.policy_id,
            policy.revision,
            &scope.scope_id,
            scope.revision,
        )
    };
    assert_eq!(first.spec.binding_revision.get(), 1);
    assert_eq!(apply(&policy).unwrap(), first);
    repository.transition(id, BindingStatus::Applying);
    assert_eq!(apply(&policy).unwrap().status, BindingStatus::Applying);
    let changed = pap
        .update_policy(
            &policy.policy_id,
            "changed",
            &policy_template("/workspace/b"),
        )
        .unwrap();
    assert_eq!(apply(&changed), Err(PapError::OperationInProgress));
    repository.transition(id, BindingStatus::ApplyFailed);
    let retry = apply(&policy).unwrap();
    assert_eq!(retry, first, "same-spec retry preserves revision");
    let updated = apply(&changed).unwrap();
    assert_eq!(updated.spec.binding_revision.get(), 2);
    let deletion = pap.delete_binding(id).unwrap();
    assert_eq!(deletion.spec, updated.spec);
    assert_eq!(deletion.status, BindingStatus::PendingDelete);
    for status in [
        BindingStatus::PendingDelete,
        BindingStatus::Deleting,
        BindingStatus::DeleteFailed,
    ] {
        if status != BindingStatus::PendingDelete {
            repository.transition(id, status);
        }
        assert_eq!(apply(&changed), Err(PapError::OperationInProgress));
        assert_eq!(
            apply(&policy),
            Err(PapError::OperationInProgress),
            "changed spec cannot cancel deletion"
        );
        if status != BindingStatus::DeleteFailed {
            assert_eq!(pap.delete_binding(id).unwrap().status, status);
        }
    }
    assert_eq!(
        pap.delete_binding(id).unwrap(),
        deletion,
        "failed deletion retries at the same revision"
    );
    assert_eq!(pap.list_bindings(100, 0).unwrap().items, vec![deletion]);
}

#[test]
fn delete_supersedes_running_apply_without_rewriting_spec() {
    let (pap, repository) = service();
    let policy = pap
        .create_policy("protect files", &policy_template("/workspace/a"))
        .unwrap();
    let scope = pap.create_scope(&ScopeSelector::Pid { pid: 4242 }).unwrap();
    let binding = pap
        .create_binding(
            &policy.policy_id,
            policy.revision,
            &scope.scope_id,
            scope.revision,
        )
        .unwrap();
    repository.transition(&binding.spec.binding_id, BindingStatus::Applying);
    let applying = repository.binding_state(&binding.spec.binding_id);
    let deletion = pap.delete_binding(&binding.spec.binding_id).unwrap();
    assert_eq!(deletion.spec, binding.spec);
    assert_eq!(deletion.status, BindingStatus::PendingDelete);
    assert_eq!(
        repository.inner.compare_exchange_binding_state(
            &applying,
            &BindingStateWrite::new({
                let mut completed = applying.clone();
                completed.binding.status.phase = BindingStatus::Ready;
                completed
            })
        ),
        Ok(WriteResult::Conflict)
    );
}

#[test]
fn binding_requires_exact_policy_and_scope_revisions() {
    let (pap, _) = service();
    let policy = pap
        .create_policy("protect files", &policy_template("/workspace/a"))
        .unwrap();
    let scope = pap.create_scope(&ScopeSelector::Pid { pid: 4242 }).unwrap();
    let missing = ResourceId::new("missing").unwrap();

    assert_eq!(
        pap.create_binding(
            &missing,
            Revision::new(1).unwrap(),
            &scope.scope_id,
            scope.revision,
        ),
        Err(PapError::ReferencedPolicyRevisionNotFound)
    );
    assert_eq!(
        pap.create_binding(
            &policy.policy_id,
            policy.revision,
            &missing,
            Revision::new(1).unwrap(),
        ),
        Err(PapError::ReferencedScopeRevisionNotFound)
    );
    assert_eq!(
        pap.update_binding(
            &missing,
            &policy.policy_id,
            policy.revision,
            &scope.scope_id,
            scope.revision,
        ),
        Err(PapError::NotFound)
    );
}

#[test]
fn existing_binding_can_reuse_embedded_sources_after_current_records_advance() {
    let (pap, repository) = service();
    let policy_v1 = pap
        .create_policy("protect files", &policy_template("/workspace/a"))
        .unwrap();
    let scope_v1 = pap.create_scope(&ScopeSelector::Pid { pid: 4242 }).unwrap();
    let binding = pap
        .create_binding(
            &policy_v1.policy_id,
            policy_v1.revision,
            &scope_v1.scope_id,
            scope_v1.revision,
        )
        .unwrap();

    pap.update_policy(
        &policy_v1.policy_id,
        "protect more files",
        &policy_template("/workspace/b"),
    )
    .unwrap();
    pap.update_scope(
        &scope_v1.scope_id,
        &ScopeSelector::CgroupId { cgroup_id: 99 },
    )
    .unwrap();

    repository.transition(&binding.spec.binding_id, BindingStatus::Applying);
    repository.transition(&binding.spec.binding_id, BindingStatus::ApplyFailed);
    let reapplied = pap
        .update_binding(
            &binding.spec.binding_id,
            &policy_v1.policy_id,
            policy_v1.revision,
            &scope_v1.scope_id,
            scope_v1.revision,
        )
        .unwrap();
    assert_eq!(reapplied.spec.binding_revision.get(), 1);
    assert_eq!(reapplied.spec.policy, policy_v1);
    assert_eq!(reapplied.spec.scope, scope_v1);
    assert_eq!(reapplied.status, BindingStatus::PendingApply);

    assert_eq!(
        pap.create_binding(
            &reapplied.spec.policy.policy_id,
            reapplied.spec.policy.revision,
            &reapplied.spec.scope.scope_id,
            reapplied.spec.scope.revision,
        ),
        Err(PapError::ReferencedPolicyRevisionNotFound),
        "a new Binding cannot select source revisions no longer current"
    );
}

#[test]
fn repository_atomically_rejects_a_binding_update_after_worker_claim() {
    let (pap, repository) = service();
    let policy = pap
        .create_policy("protect files", &policy_template("/workspace/a"))
        .unwrap();
    let scope = pap.create_scope(&ScopeSelector::Pid { pid: 4242 }).unwrap();
    let binding = pap
        .create_binding(
            &policy.policy_id,
            policy.revision,
            &scope.scope_id,
            scope.revision,
        )
        .unwrap();
    let mut stale_replacement = binding.clone();
    stale_replacement.spec.binding_revision = Revision::new(2).unwrap();

    repository.transition(&binding.spec.binding_id, BindingStatus::Applying);

    assert_eq!(
        repository.update_binding(Some(&binding), &stale_replacement),
        Err(PapError::Conflict),
        "the repository gate must close the service-read/worker-claim race"
    );
}

#[test]
fn repository_rejects_stale_worker_revision_while_status_is_unchanged() {
    let (pap, repository) = service();
    let policy = pap
        .create_policy("protect files", &policy_template("/workspace/a"))
        .unwrap();
    let scope = pap.create_scope(&ScopeSelector::Pid { pid: 4242 }).unwrap();
    let binding = pap
        .create_binding(
            &policy.policy_id,
            policy.revision,
            &scope.scope_id,
            scope.revision,
        )
        .unwrap();
    let pending = repository.binding_state(&binding.spec.binding_id);
    let changed = pap
        .update_policy(
            &policy.policy_id,
            "changed",
            &policy_template("/workspace/b"),
        )
        .unwrap();
    let updated = pap
        .update_binding(
            &binding.spec.binding_id,
            &changed.policy_id,
            changed.revision,
            &scope.scope_id,
            scope.revision,
        )
        .unwrap();
    assert_eq!(updated.status, pending.binding.status.phase);
    assert_ne!(
        updated.spec.binding_revision,
        pending.binding.spec.binding_revision
    );
    let mut claimed = pending.clone();
    claimed.binding.status.phase = BindingStatus::Applying;
    assert_eq!(
        repository
            .inner
            .compare_exchange_binding_state(&pending, &BindingStateWrite::new(claimed)),
        Ok(WriteResult::Conflict)
    );
    assert_eq!(pap.get_binding(&binding.spec.binding_id).unwrap(), updated);
}

#[test]
fn compiler_output_identity_is_checked_before_storage() {
    let repository = Arc::new(FakeRepository::default());
    let compiler = Arc::new(FixtureCompiler {
        mismatch_identity: true,
    });
    let pap = PapService::new(repository, compiler);

    let error = pap
        .create_policy("protect files", &policy_template("/workspace/a"))
        .unwrap_err();
    let PapError::InvalidPolicy(error) = error else {
        panic!("expected invalid compiler output");
    };
    assert_eq!(error.path, "canonicalPolicy.policyId");
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
    exhausted.canonical_policy.revision = maximum;
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
        let pap = pap.with_reconcile_enqueuer(Arc::new(Reject));
        let policy = pap
            .create_policy("test", &policy_template("/workspace/a"))
            .unwrap();
        let scope = pap.create_scope(&ScopeSelector::Pid { pid: 10 }).unwrap();
        let error = pap
            .create_binding(
                &policy.policy_id,
                policy.revision,
                &scope.scope_id,
                scope.revision,
            )
            .unwrap_err();
        let PapError::SchedulingRejected { id, .. } = error else {
            panic!("expected scheduling rejection")
        };
        let state = repo.binding_state(&id);
        assert_eq!(state.binding.status.phase, BindingStatus::PendingApply);
        assert_eq!(state.binding.status.error, None);
    }
}
