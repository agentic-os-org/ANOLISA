//! Test-only fixture setup on the production connection, schema and transactions.

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;

use asc_pap::PapError;
use asc_policy_repository::{BindingIntentReceipt, BindingStateSnapshot, StoreError};
use asc_policy_types::{
    Validate,
    binding::{BindingStatus, BindingView},
    scope::{PreparedScope, ScopeStatus},
};

use crate::{RepositoryError, SqlitePolicyRepository, records};

impl SqlitePolicyRepository {
    /// Opens an isolated temporary on-disk database with production validation.
    /// The repository owns its private directory and removes it on drop.
    /// # Errors
    /// Returns directory creation or repository initialization failures.
    pub fn temporary() -> Result<Self, StoreError> {
        let directory = tempfile::Builder::new()
            .prefix("asc-policy-test-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in("/tmp")
            .map_err(|_| StoreError::Io)?;
        let mut repository = Self::open(&directory.path().join("policy-state.db"))
            .map_err(RepositoryError::store)?;
        repository.test_directory = Some(directory);
        Ok(repository)
    }

    /// Seeds frozen reconciler states, including their owning Scope snapshots.
    /// Fixture setup alone bypasses discovery; subsequent operations use production code.
    /// # Errors
    /// Rejects invalid records, duplicate identities and database failures.
    pub fn with_binding_states(records: Vec<BindingStateSnapshot>) -> Result<Self, StoreError> {
        let repository = Self::temporary()?;
        repository
            .access(true, |tx| {
                let mut scopes = BTreeMap::new();
                for record in &records {
                    record.binding.validate().map_err(|_| StoreError::Invalid)?;
                    let owner = &record.binding.spec.scope;
                    let scope =
                        scopes
                            .entry(owner.scope_id.clone())
                            .or_insert_with(|| PreparedScope {
                                scope_id: owner.scope_id.clone(),
                                selector: owner.selector.clone(),
                                policy_snapshots: Vec::new(),
                                status: ScopeStatus::Active,
                            });
                    if scope.selector != owner.selector {
                        return Err(StoreError::Invalid.into());
                    }
                    if !scope.policy_snapshots.contains(&record.binding.spec.policy) {
                        scope
                            .policy_snapshots
                            .push(record.binding.spec.policy.clone());
                    }
                }
                for scope in scopes.values() {
                    scope.validate().map_err(|_| StoreError::Invalid)?;
                    records::insert_scope(tx, scope)?;
                }
                for record in records {
                    if records::read_binding(tx, &record.binding.spec.binding_id)?.is_some() {
                        return Err(StoreError::Invalid.into());
                    }
                    records::save_binding(tx, &record)?;
                }
                records::validate_records(tx)
            })
            .map_err(RepositoryError::store)?;
        Ok(repository)
    }

    /// Admits synthetic changed-spec intents for lower-level CAS regression tests.
    /// Product PAP only admits Bindings through Scope discovery.
    /// # Errors
    /// Rejects stale expectations, invalid revisions, lifecycle conflicts or storage failures.
    pub fn admit_binding_for_test(
        &self,
        expected: Option<&BindingIntentReceipt>,
        binding: &BindingView,
    ) -> Result<BindingView, PapError> {
        binding.validate().map_err(PapError::InvalidBinding)?;
        if !matches!(
            binding.status.phase,
            BindingStatus::PendingApply | BindingStatus::PendingDelete
        ) {
            return Err(PapError::Conflict);
        }
        self.access(true, |tx| {
            let current = records::read_binding(tx, &binding.spec.binding_id)?;
            if expected.is_some() && current.is_none() {
                return Err(PapError::NotFound.into());
            }
            if current.as_ref().map(|s| (&s.binding, s.status_version))
                != expected.map(|s| (&s.binding, s.status_version))
            {
                return Err(PapError::Conflict.into());
            }
            if binding.status == BindingStatus::PendingApply {
                let owner = records::read_scope(tx, &binding.spec.scope.scope_id)?
                    .ok_or(PapError::NotFound)?
                    .scope;
                if owner.status != ScopeStatus::Active {
                    return Err(PapError::OperationInProgress.into());
                }
                if let Some(current) = &current {
                    if records::read_scope(tx, &current.binding.spec.scope.scope_id)?
                        .is_none_or(|s| s.scope.status != ScopeStatus::Active)
                    {
                        return Err(PapError::OperationInProgress.into());
                    }
                } else if owner.selector != binding.spec.scope.selector
                    || !owner.policy_snapshots.contains(&binding.spec.policy)
                {
                    return Err(PapError::Conflict.into());
                }
            }
            let mut next = if let Some(current) = current {
                if current.binding == *binding {
                    return Ok(binding.clone());
                }
                let same_spec = current.binding.spec.policy == binding.spec.policy
                    && current.binding.spec.scope == binding.spec.scope;
                let permitted = if binding.status == BindingStatus::PendingDelete {
                    same_spec && current.binding.status.request_delete() == binding.status
                } else if same_spec {
                    current.binding.status.request_apply().ok() == Some(binding.status.phase)
                } else {
                    current.binding.status.request_apply().is_ok()
                        && current.binding.status != BindingStatus::Applying
                };
                if !permitted {
                    return Err(PapError::OperationInProgress.into());
                }
                let revision = current.binding.spec.binding_revision;
                if (same_spec && revision != binding.spec.binding_revision)
                    || (!same_spec && revision.checked_next() != Ok(binding.spec.binding_revision))
                {
                    return Err(PapError::Conflict.into());
                }
                current
            } else {
                if binding.spec.binding_revision.get() != 1
                    || binding.status != BindingStatus::PendingApply
                {
                    return Err(PapError::Conflict.into());
                }
                BindingStateSnapshot {
                    binding: binding.clone(),
                    status_version: 1,
                    deployments: Vec::new(),
                }
            };
            records::change_status(&mut next, binding.status.clone())?;
            next.binding = binding.clone();
            records::save_binding(tx, &next)?;
            Ok(binding.clone())
        })
        .map_err(RepositoryError::pap)
    }
}
