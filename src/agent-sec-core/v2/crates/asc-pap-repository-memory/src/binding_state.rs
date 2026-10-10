use crate::{BindingStateData, ProcessLocalPapRepository, State};
use asc_foundation_types::ResourceId;
use asc_policy_repository::{
    BindingStateRepository, BindingStateSnapshot, BindingStateWrite, StoreError, WriteResult,
};
use asc_policy_types::{error::Validate, target::Presence};
use std::sync::Mutex;

impl ProcessLocalPapRepository {
    /// Seeds complete records for local composition/contract tests. This is not
    /// a disk loader or evidence of crash recovery. Duplicate identities fail.
    /// # Errors
    /// Returns invalid on malformed records or duplicate target identities.
    pub fn with_binding_states(records: Vec<BindingStateSnapshot>) -> Result<Self, StoreError> {
        let mut state = State::default();
        for record in records {
            record.binding.validate().map_err(|_| StoreError::Invalid)?;
            if record.status_version < 1 {
                return Err(StoreError::Invalid);
            }
            for (index, deployment) in record.deployments.iter().enumerate() {
                if deployment.presence == Presence::Absent
                    || record.deployments[..index]
                        .iter()
                        .any(|d| d.target.same_identity(&deployment.target))
                {
                    return Err(StoreError::Invalid);
                }
            }
            let owner = &record.binding.spec.scope;
            let scope = state
                .scopes
                .entry(owner.scope_id.to_string())
                .or_insert_with(|| asc_policy_types::scope::PreparedScope {
                    scope_id: owner.scope_id.clone(),
                    selector: owner.selector.clone(),
                    policy_snapshots: Vec::new(),
                    status: asc_policy_types::scope::ScopeStatus::Active,
                });
            if !scope.policy_snapshots.contains(&record.binding.spec.policy) {
                scope
                    .policy_snapshots
                    .push(record.binding.spec.policy.clone());
            }
            let id = record.binding.spec.binding_id.to_string();
            if state
                .bindings
                .insert(id.clone(), record.binding.clone())
                .is_some()
            {
                return Err(StoreError::Invalid);
            }
            state.binding_states.insert(
                id,
                BindingStateData {
                    status_version: record.status_version,
                    deployments: record.deployments,
                    last_write: None,
                },
            );
        }
        Ok(Self {
            state: Mutex::new(state),
        })
    }
}

impl BindingStateRepository for ProcessLocalPapRepository {
    fn get_binding_state(
        &self,
        id: &ResourceId,
    ) -> Result<Option<BindingStateSnapshot>, StoreError> {
        let state = self.state.lock().map_err(|_| StoreError::Unavailable)?;
        Ok(snapshot(&state, id.as_str()))
    }
    fn compare_exchange_binding_state(
        &self,
        expected: &BindingStateSnapshot,
        write: &BindingStateWrite,
    ) -> Result<WriteResult, StoreError> {
        let id = expected.binding.spec.binding_id.as_str();
        let mut state = self.state.lock().map_err(|_| StoreError::Unavailable)?;
        let digest = asc_policy_repository::write_digest(expected, write)?;
        if let Some((write_id, previous_digest, receipt)) = state
            .binding_states
            .get(id)
            .and_then(|s| s.last_write.as_ref())
            && *write_id == write.write_id
        {
            return if *previous_digest == digest {
                Ok(WriteResult::AlreadyApplied(*receipt))
            } else {
                Err(StoreError::Invalid)
            };
        }
        let Some(current) = snapshot(&state, id) else {
            return Ok(if write.next.is_none() {
                WriteResult::AlreadyApplied(asc_policy_repository::WriteReceipt {
                    status_version: None,
                    status_applied: true,
                })
            } else {
                WriteResult::Conflict
            });
        };
        let (next, result) = asc_policy_repository::apply_binding_write(&current, expected, write)?;
        let WriteResult::Applied(receipt) = result else {
            return Ok(result);
        };
        if let Some(next) = next {
            state.bindings.insert(id.to_owned(), next.binding);
            state.binding_states.insert(
                id.to_owned(),
                BindingStateData {
                    status_version: next.status_version,
                    deployments: next.deployments,
                    last_write: Some((write.write_id, digest, receipt)),
                },
            );
        } else {
            let scope_id = current.binding.spec.scope.scope_id.to_string();
            state.bindings.remove(id);
            state.binding_states.remove(id);
            crate::finalize_scope(&mut state, &scope_id);
        }
        Ok(result)
    }
}
fn snapshot(state: &State, id: &str) -> Option<BindingStateSnapshot> {
    let binding = state.bindings.get(id)?.clone();
    let data = state.binding_states.get(id);
    Some(BindingStateSnapshot {
        status_version: data.map_or(1, |d| d.status_version),
        binding,
        deployments: data.map(|d| d.deployments.clone()).unwrap_or_default(),
    })
}

impl asc_policy_repository::BindingReconcileCatalog for ProcessLocalPapRepository {
    fn scan_reconciliation(
        &self,
        after: Option<&ResourceId>,
        limit: usize,
    ) -> Result<Vec<asc_policy_repository::ReconcileCandidate>, StoreError> {
        if limit == 0 || limit > 1000 {
            return Err(StoreError::Invalid);
        }
        let state = self.state.lock().map_err(|_| StoreError::Unavailable)?;
        Ok(state
            .bindings
            .range((
                after.map_or(std::ops::Bound::Unbounded, |id| {
                    std::ops::Bound::Excluded(id.to_string())
                }),
                std::ops::Bound::Unbounded,
            ))
            .take(limit)
            .map(|(_id, binding)| asc_policy_repository::ReconcileCandidate {
                id: binding.spec.binding_id.clone(),
                status: binding.status.phase,
            })
            .collect())
    }
}
