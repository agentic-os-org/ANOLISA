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
            for (index, deployment) in record.deployments.iter().enumerate() {
                if deployment.presence == Presence::Absent
                    || record.deployments[..index]
                        .iter()
                        .any(|d| d.target.same_identity(&deployment.target))
                {
                    return Err(StoreError::Invalid);
                }
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
        if let Some(last) = state
            .binding_states
            .get(id)
            .and_then(|r| r.last_write.as_ref())
            && last.write_id == write.write_id
        {
            return if last == write {
                Ok(WriteResult::AlreadyApplied)
            } else {
                Err(StoreError::Invalid)
            };
        }
        let current = snapshot(&state, id);
        if current.is_none() && write.next.is_none() {
            return Ok(WriteResult::AlreadyApplied);
        }
        let Some(current) = current else {
            return Ok(WriteResult::Conflict);
        };
        let patch = write.next.as_ref();
        if current.binding.spec.binding_revision != expected.binding.spec.binding_revision
            || current.binding.status.phase != expected.binding.status.phase
            || (patch.is_none_or(|p| p.status.is_some())
                && current.binding.status != expected.binding.status)
            || (patch.is_none_or(|p| p.deployments.is_some())
                && current.deployments != expected.deployments)
        {
            return Ok(WriteResult::Conflict);
        }
        if let Some(next) = &write.next {
            // Validate before mutating either map.
            if next
                .status
                .as_ref()
                .is_some_and(|s| s.phase == asc_policy_types::binding::BindingStatus::Deleted)
            {
                return Err(StoreError::Invalid);
            }
            if let Some(deployments) = &next.deployments {
                for (index, deployment) in deployments.iter().enumerate() {
                    if deployment.presence == Presence::Absent
                        || deployments[..index]
                            .iter()
                            .any(|d| d.target.same_identity(&deployment.target))
                    {
                        return Err(StoreError::Invalid);
                    }
                }
            }
            if let Some(status) = &next.status {
                state
                    .bindings
                    .get_mut(id)
                    .ok_or(StoreError::Invalid)?
                    .status = status.clone();
            }
            let data = state.binding_states.entry(id.to_owned()).or_default();
            if let Some(deployments) = &next.deployments {
                data.deployments.clone_from(deployments);
            }
            data.last_write = Some(write.clone());
        } else {
            state.bindings.remove(id);
            state.binding_states.remove(id);
        }
        Ok(WriteResult::Applied)
    }
}
fn snapshot(state: &State, id: &str) -> Option<BindingStateSnapshot> {
    let binding = state.bindings.get(id)?.clone();
    let data = state.binding_states.get(id);
    Some(BindingStateSnapshot {
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
