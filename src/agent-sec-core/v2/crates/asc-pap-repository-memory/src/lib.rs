//! **TEMPORARY PROCESS-LOCAL IMPLEMENTATION -- NO DURABLE PERSISTENCE.**
//!
//! This process-local adapter exists only to make the PAP daemon request path
//! runnable before the durable Repository work package lands. Its internal
//! implementation is replaceable and must not be treated as a durable storage
//! design. Reconciliation transactions are reviewed/tested as logical atomicity.
//! All state is lost when the daemon restarts.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};

mod binding_state;

use asc_foundation_types::{ResourceId, Revision};
use asc_pap::{Page, PapError, PapRepository, PolicyRevisionState, ScopeRevisionState};
use asc_policy_types::binding::{BindingStatus, BindingView};
use asc_policy_types::policy::PreparedPolicy;
use asc_policy_types::scope::PreparedScope;

/// Process-local PAP Repository used until durable persistence is integrated.
///
/// This adapter implements the current-record Repository contract but loses all
/// state on daemon restart. It is an explicitly replaceable composition-root
/// dependency, not production persistence evidence.
#[derive(Debug, Default)]
pub struct ProcessLocalPapRepository {
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    policy_heads: BTreeMap<String, Revision>,
    policies: BTreeMap<String, PreparedPolicy>,
    scope_heads: BTreeMap<String, Revision>,
    scopes: BTreeMap<String, PreparedScope>,
    bindings: BTreeMap<String, BindingView>,
    binding_states: BTreeMap<String, BindingStateData>,
}

#[derive(Debug, Default, Clone)]
struct BindingStateData {
    deployments: Vec<asc_policy_repository::Deployment>,
    // Latest atomic write receipt, independent of reconciliation decisions.
    last_write: Option<asc_policy_repository::BindingStateWrite>,
}

impl ProcessLocalPapRepository {
    fn lock(&self) -> Result<MutexGuard<'_, State>, PapError> {
        self.state.lock().map_err(|_| PapError::Persistence)
    }
}

impl PapRepository for ProcessLocalPapRepository {
    fn put_policy(&self, policy: &PreparedPolicy) -> Result<PreparedPolicy, PapError> {
        let mut state = self.lock()?;
        let id = policy.policy_id.as_str().to_owned();
        if let Some(existing) = state.policies.get(&id)
            && existing.revision == policy.revision
        {
            return if existing == policy {
                Ok(existing.clone())
            } else {
                Err(PapError::Conflict)
            };
        }
        if !is_next_revision(state.policy_heads.get(&id).copied(), policy.revision) {
            return Err(PapError::Conflict);
        }
        state.policies.insert(id.clone(), policy.clone());
        state.policy_heads.insert(id, policy.revision);
        Ok(policy.clone())
    }

    fn get_policy_revision_state(
        &self,
        id: &ResourceId,
    ) -> Result<Option<PolicyRevisionState>, PapError> {
        let state = self.lock()?;
        Ok(state
            .policy_heads
            .get(id.as_str())
            .copied()
            .map(|last_allocated_revision| PolicyRevisionState {
                last_allocated_revision,
                current: state.policies.get(id.as_str()).cloned(),
            }))
    }

    fn get_policy(&self, id: &ResourceId, revision: Revision) -> Result<PreparedPolicy, PapError> {
        self.lock()?
            .policies
            .get(id.as_str())
            .filter(|policy| policy.revision == revision)
            .cloned()
            .ok_or(PapError::NotFound)
    }

    fn list_policies(&self, limit: u32, offset: u32) -> Result<Page<PreparedPolicy>, PapError> {
        let state = self.lock()?;
        Ok(page(state.policies.values(), limit, offset))
    }

    fn delete_policy_revision(
        &self,
        id: &ResourceId,
        revision: Revision,
    ) -> Result<PreparedPolicy, PapError> {
        let mut state = self.lock()?;
        if state
            .policies
            .get(id.as_str())
            .is_none_or(|policy| policy.revision != revision)
        {
            return Err(PapError::NotFound);
        }
        state
            .policies
            .remove(id.as_str())
            .ok_or(PapError::Persistence)
    }

    fn put_scope(&self, scope: &PreparedScope) -> Result<PreparedScope, PapError> {
        let mut state = self.lock()?;
        let id = scope.scope_id.as_str().to_owned();
        if let Some(existing) = state.scopes.get(&id)
            && existing.revision == scope.revision
        {
            return if existing == scope {
                Ok(existing.clone())
            } else {
                Err(PapError::Conflict)
            };
        }
        if !is_next_revision(state.scope_heads.get(&id).copied(), scope.revision) {
            return Err(PapError::Conflict);
        }
        state.scopes.insert(id.clone(), scope.clone());
        state.scope_heads.insert(id, scope.revision);
        Ok(scope.clone())
    }

    fn get_scope_revision_state(
        &self,
        id: &ResourceId,
    ) -> Result<Option<ScopeRevisionState>, PapError> {
        let state = self.lock()?;
        Ok(state
            .scope_heads
            .get(id.as_str())
            .copied()
            .map(|last_allocated_revision| ScopeRevisionState {
                last_allocated_revision,
                current: state.scopes.get(id.as_str()).cloned(),
            }))
    }

    fn get_scope(&self, id: &ResourceId, revision: Revision) -> Result<PreparedScope, PapError> {
        self.lock()?
            .scopes
            .get(id.as_str())
            .filter(|scope| scope.revision == revision)
            .cloned()
            .ok_or(PapError::NotFound)
    }

    fn list_scopes(&self, limit: u32, offset: u32) -> Result<Page<PreparedScope>, PapError> {
        let state = self.lock()?;
        Ok(page(state.scopes.values(), limit, offset))
    }

    fn delete_scope_revision(
        &self,
        id: &ResourceId,
        revision: Revision,
    ) -> Result<PreparedScope, PapError> {
        let mut state = self.lock()?;
        if state
            .scopes
            .get(id.as_str())
            .is_none_or(|scope| scope.revision != revision)
        {
            return Err(PapError::NotFound);
        }
        state
            .scopes
            .remove(id.as_str())
            .ok_or(PapError::Persistence)
    }

    fn update_binding(
        &self,
        expected: Option<&BindingView>,
        binding: &BindingView,
    ) -> Result<BindingView, PapError> {
        if !matches!(
            binding.status.phase,
            BindingStatus::PendingApply | BindingStatus::PendingDelete
        ) {
            return Err(PapError::Conflict);
        }
        let mut state = self.lock()?;
        let id = binding.spec.binding_id.as_str().to_owned();
        let current = state.bindings.get(&id);
        if expected.is_some() && current.is_none() {
            return Err(PapError::NotFound);
        }
        if current.map(|b| (&b.spec, &b.status)) != expected.map(|b| (&b.spec, &b.status)) {
            return Err(PapError::Conflict);
        }
        if let Some(current) = current {
            if current.spec == binding.spec && current.status == binding.status {
                return Ok(current.clone());
            }
            let same_spec = current.spec.policy == binding.spec.policy
                && current.spec.scope == binding.spec.scope;
            let permitted = if binding.status == BindingStatus::PendingDelete {
                same_spec && current.status.request_delete() == binding.status
            } else if same_spec {
                current.status.request_apply().ok() == Some(binding.status.phase)
            } else {
                current.status.request_apply().is_ok() && current.status != BindingStatus::Applying
            };
            if !permitted {
                return Err(PapError::OperationInProgress);
            }
            let valid_revision = if same_spec {
                current.spec.binding_revision == binding.spec.binding_revision
            } else {
                is_next_revision(
                    Some(current.spec.binding_revision),
                    binding.spec.binding_revision,
                )
            };
            if !valid_revision {
                return Err(PapError::Conflict);
            }
        } else if binding.spec.binding_revision.get() != 1
            || binding.status != BindingStatus::PendingApply
        {
            return Err(PapError::Conflict);
        }
        state.bindings.insert(id, binding.clone());
        Ok(binding.clone())
    }

    fn fail_pending_binding(
        &self,
        expected: &BindingView,
        reason: asc_pap::EnqueueError,
    ) -> Result<bool, PapError> {
        let failed = match expected.status.phase {
            BindingStatus::PendingApply => BindingStatus::ApplyFailed,
            BindingStatus::PendingDelete => BindingStatus::DeleteFailed,
            _ => return Err(PapError::Conflict),
        };
        let mut state = self.lock()?;
        let id = expected.spec.binding_id.as_str();
        let Some(current) = state.bindings.get_mut(id) else {
            return Ok(false);
        };
        if current.spec.binding_revision != expected.spec.binding_revision
            || current.status != expected.status
        {
            return Ok(false);
        }
        current.status = failed.into();
        current.status.error = Some(reason.failure());
        let data = state.binding_states.entry(id.to_owned()).or_default();

        // Invalidate an earlier worker write replay after this lifecycle change.
        data.last_write = None;
        Ok(true)
    }

    fn get_binding(&self, id: &ResourceId) -> Result<BindingView, PapError> {
        let state = self.lock()?;
        let binding = state.bindings.get(id.as_str()).ok_or(PapError::NotFound)?;
        let mut view = binding.clone();
        project_binding_error(&state, &mut view);
        Ok(view)
    }

    fn list_bindings(&self, limit: u32, offset: u32) -> Result<Page<BindingView>, PapError> {
        let state = self.lock()?;
        let mut result = page(state.bindings.values(), limit, offset);
        for binding in &mut result.items {
            project_binding_error(&state, binding);
        }
        Ok(result)
    }
}

fn project_binding_error(_state: &State, view: &mut BindingView) {
    view.status.error = view
        .status
        .error
        .as_ref()
        .map(|error| asc_policy_types::target::Failure::new(error.kind, &error.code));
}

fn is_next_revision(current: Option<Revision>, candidate: Revision) -> bool {
    match current {
        None => candidate.get() == 1,
        Some(current) => current.checked_next() == Ok(candidate),
    }
}

fn page<'a, T: Clone + 'a>(
    items: impl ExactSizeIterator<Item = &'a T>,
    limit: u32,
    offset: u32,
) -> Page<T> {
    let total = u64::try_from(items.len()).unwrap_or(u64::MAX);
    let offset = usize::try_from(offset).unwrap_or(usize::MAX);
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    Page {
        items: items.skip(offset).take(limit).cloned().collect(),
        total,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use asc_pap::PapService;
    use asc_policy_engine::PolicyTemplateCompiler;
    use asc_policy_types::authoring::PolicyTemplate;
    use asc_policy_types::binding::BindingStatus;
    use asc_policy_types::scope::ScopeSelector;

    use super::*;

    #[derive(Debug)]
    struct CloneTracked<'a> {
        value: u32,
        clones: &'a AtomicUsize,
    }

    impl Clone for CloneTracked<'_> {
        fn clone(&self) -> Self {
            self.clones.fetch_add(1, Ordering::Relaxed);
            Self {
                value: self.value,
                clones: self.clones,
            }
        }
    }

    #[test]
    fn pagination_clones_only_the_selected_records() {
        let clones = AtomicUsize::new(0);
        let records: Vec<_> = (0..5)
            .map(|value| CloneTracked {
                value,
                clones: &clones,
            })
            .collect();

        let selected = page(records.iter(), 2, 2);

        assert_eq!(selected.total, 5);
        assert_eq!(
            selected
                .items
                .iter()
                .map(|record| record.value)
                .collect::<Vec<_>>(),
            [2, 3]
        );
        assert_eq!(clones.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn repository_runs_the_current_pap_request_slice_without_a_reconciler() {
        let repository = Arc::new(ProcessLocalPapRepository::default());
        let pap = PapService::new(Arc::clone(&repository), Arc::new(PolicyTemplateCompiler));
        let policy = pap
            .create_policy(
                "protect files",
                &PolicyTemplate::PreventFileDeletion {
                    files: vec!["/workspace/important".to_owned()],
                },
            )
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

        assert_eq!(binding.status, BindingStatus::PendingApply);
        assert_eq!(pap.list_policies(10, 0).unwrap().total, 1);
        assert_eq!(pap.list_scopes(10, 0).unwrap().total, 1);
        assert_eq!(pap.list_bindings(10, 0).unwrap().items, [binding]);
        assert_eq!(
            ProcessLocalPapRepository::default()
                .list_policies(10, 0)
                .unwrap()
                .total,
            0,
            "a new process-local Repository intentionally has no prior state"
        );
    }
}
