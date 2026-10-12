//! PAP transactions; notifications and discovery threads run after commit.
use crate::{
    RepositoryError, SqlitePolicyRepository,
    records::{
        binding_ids, change_status, encode, finalize_scope, id, insert_scope, page, read_binding,
        read_policy, read_scope, receipt, save_binding,
    },
};
use asc_foundation_types::{ResourceId, Revision};
use asc_pap::{Page, PapError, PapRepository, PolicyRevisionState, ScopeDiscoverySeed};
use asc_policy_repository::{BindingIntentReceipt, BindingStateSnapshot, StoreError};
use asc_policy_types::{
    Validate,
    binding::{BindingScope, BindingStatus, BindingView, PreparedBinding},
    policy::PreparedPolicy,
    process_discovery::ProcessIdentity,
    scope::{PreparedScope, ScopeSelector, ScopeStatus},
};
use rusqlite::params;
use std::collections::BTreeSet;

impl PapRepository for SqlitePolicyRepository {
    fn put_policy(&self, policy: &PreparedPolicy) -> Result<PreparedPolicy, PapError> {
        policy.validate().map_err(PapError::InvalidPolicy)?;
        self.access(true, |tx| {
            let encoded = encode(policy)?;
            if encoded.len() > 1024 * 1024 {
                return Err(PapError::InvalidPolicy(
                    asc_policy_types::error::ValidationError::new(
                        "template",
                        "serialized policy exceeds 1 MiB",
                    ),
                )
                .into());
            }
            let current = read_policy(tx, &policy.policy_id)?;
            if current.as_ref().and_then(|s| s.current.as_ref()) == Some(policy) {
                return Ok(policy.clone());
            }
            let next = match current {
                Some(state) => state.last_allocated_revision.checked_next(),
                None => Revision::new(1),
            }
            .map_err(|_| StoreError::Invalid)?;
            if policy.revision != next {
                return Err(PapError::Conflict.into());
            }
            tx.execute(
                "INSERT INTO policies(policy_id,last_allocated_revision,current_json)
                 VALUES (?1,?2,?3) ON CONFLICT(policy_id) DO UPDATE SET
                 last_allocated_revision=excluded.last_allocated_revision,
                 current_json=excluded.current_json",
                params![policy.policy_id.as_str(), policy.revision.get(), encoded],
            )?;
            Ok(policy.clone())
        })
        .map_err(RepositoryError::pap)
    }
    fn get_policy_revision_state(
        &self,
        key: &ResourceId,
    ) -> Result<Option<PolicyRevisionState>, PapError> {
        self.access(false, |tx| read_policy(tx, key))
            .map_err(RepositoryError::pap)
    }
    fn get_policy(&self, key: &ResourceId, revision: Revision) -> Result<PreparedPolicy, PapError> {
        self.get_policy_revision_state(key)?
            .and_then(|s| s.current)
            .filter(|p| p.revision == revision)
            .ok_or(PapError::NotFound)
    }
    fn list_policies(&self, limit: u32, offset: u32) -> Result<Page<PreparedPolicy>, PapError> {
        self.access(false, |tx| {
            let total = tx.query_row(
                "SELECT count(*) FROM policies WHERE current_json IS NOT NULL",
                [],
                |r| r.get(0),
            )?;
            let keys: Vec<String> = tx
                .prepare(
                    "SELECT policy_id FROM policies WHERE current_json IS NOT NULL
                 ORDER BY policy_id LIMIT ?1 OFFSET ?2",
                )?
                .query_map(params![limit, offset], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            page(
                total,
                keys.into_iter().map(|key| {
                    read_policy(tx, &id(key)?)?
                        .and_then(|p| p.current)
                        .ok_or(StoreError::Corrupt.into())
                }),
            )
        })
        .map_err(RepositoryError::pap)
    }
    fn delete_policy_revision(
        &self,
        key: &ResourceId,
        revision: Revision,
    ) -> Result<PreparedPolicy, PapError> {
        self.access(true, |tx| {
            let current = read_policy(tx, key)?
                .and_then(|p| p.current)
                .filter(|p| p.revision == revision)
                .ok_or(PapError::NotFound)?;
            tx.execute(
                "UPDATE policies SET current_json=NULL WHERE policy_id=?1",
                [key.as_str()],
            )?;
            Ok(current)
        })
        .map_err(RepositoryError::pap)
    }
    fn put_scope(&self, scope: &PreparedScope) -> Result<PreparedScope, PapError> {
        scope.validate().map_err(PapError::InvalidScope)?;
        self.access(true, |tx| {
            if encode(scope)?.len() > 1024 * 1024 {
                return Err(
                    PapError::InvalidScope(asc_policy_types::error::ValidationError::new(
                        "policyTemplates",
                        "serialized assignment exceeds 1 MiB",
                    ))
                    .into(),
                );
            }
            if scope.status != ScopeStatus::Active || read_scope(tx, &scope.scope_id)?.is_some() {
                return Err(PapError::Conflict.into());
            }
            for policy in &scope.policy_snapshots {
                if read_policy(tx, &policy.policy_id)?
                    .and_then(|s| s.current)
                    .as_ref()
                    != Some(policy)
                {
                    return Err(PapError::ReferencedPolicyRevisionNotFound.into());
                }
            }
            let count: i64 = tx.query_row(
                "SELECT count(*) FROM scopes WHERE phase='ACTIVE'",
                [],
                |r| r.get(0),
            )?;
            if count >= 32 {
                return Err(PapError::Unavailable.into());
            }
            insert_scope(tx, scope)?;
            Ok(scope.clone())
        })
        .map_err(RepositoryError::pap)
    }
    fn get_scope(&self, key: &ResourceId) -> Result<PreparedScope, PapError> {
        self.access(false, |tx| {
            Ok(read_scope(tx, key)?.ok_or(PapError::NotFound)?.scope)
        })
        .map_err(RepositoryError::pap)
    }
    fn list_scopes(&self, limit: u32, offset: u32) -> Result<Page<PreparedScope>, PapError> {
        self.access(false, |tx| {
            let total = tx.query_row("SELECT count(*) FROM scopes", [], |r| r.get(0))?;
            let keys: Vec<String> = tx
                .prepare("SELECT scope_id FROM scopes ORDER BY scope_id LIMIT ?1 OFFSET ?2")?
                .query_map(params![limit, offset], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            page(
                total,
                keys.into_iter()
                    .map(|key| Ok(read_scope(tx, &id(key)?)?.ok_or(StoreError::Corrupt)?.scope)),
            )
        })
        .map_err(RepositoryError::pap)
    }
    fn scan_scopes(
        &self,
        after: Option<&ResourceId>,
        limit: usize,
    ) -> Result<Vec<PreparedScope>, PapError> {
        if limit == 0 || limit > 1000 {
            return Err(PapError::Persistence);
        }
        self.access(false, |tx| {
            let keys: Vec<String> = tx
                .prepare(
                    "SELECT scope_id FROM scopes WHERE (?1 IS NULL OR scope_id>?1)
                 ORDER BY scope_id LIMIT ?2",
                )?
                .query_map(params![after.map(ResourceId::as_str), limit], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            keys.into_iter()
                .map(|key| Ok(read_scope(tx, &id(key)?)?.ok_or(StoreError::Corrupt)?.scope))
                .collect()
        })
        .map_err(RepositoryError::pap)
    }
    fn scope_discovery_seed(&self, key: &ResourceId) -> Result<ScopeDiscoverySeed, PapError> {
        self.access(false, |tx| {
            let scope = read_scope(tx, key)?.ok_or(PapError::NotFound)?;
            let mut instances = BTreeSet::new();
            for key in binding_ids(tx, key)? {
                let binding = read_binding(tx, &key)?.ok_or(StoreError::Corrupt)?.binding;
                if matches!(
                    binding.status.phase,
                    BindingStatus::PendingApply
                        | BindingStatus::Applying
                        | BindingStatus::Ready
                        | BindingStatus::ApplyFailed
                ) {
                    instances.insert(binding.spec.scope.process);
                }
            }
            Ok(ScopeDiscoverySeed {
                scope: scope.scope,
                pinned_process: scope.pin,
                instances: instances.into_iter().collect(),
            })
        })
        .map_err(RepositoryError::pap)
    }
    fn begin_scope_delete(&self, key: &ResourceId) -> Result<Option<PreparedScope>, PapError> {
        self.access(true, |tx| {
            let Some(mut scope) = read_scope(tx, key)? else {
                return Ok(None);
            };
            scope.scope.status = ScopeStatus::Deleting;
            tx.execute(
                "UPDATE scopes SET phase='DELETING' WHERE scope_id=?1",
                [key.as_str()],
            )?;
            Ok(Some(scope.scope))
        })
        .map_err(RepositoryError::pap)
    }
    fn finish_scope_discovery(
        &self,
        key: &ResourceId,
    ) -> Result<Vec<BindingIntentReceipt>, PapError> {
        self.access(true, |tx| {
            let Some(scope) = read_scope(tx, key)? else {
                return Ok(Vec::new());
            };
            if scope.scope.status != ScopeStatus::Deleting {
                return Err(PapError::Conflict.into());
            }
            tx.execute(
                "UPDATE scopes SET discovery_stopped=1 WHERE scope_id=?1",
                [key.as_str()],
            )?;
            let mut changed = Vec::new();
            for key in binding_ids(tx, key)? {
                let mut binding = read_binding(tx, &key)?.ok_or(StoreError::Corrupt)?;
                if retire(&mut binding)? {
                    save_binding(tx, &binding)?;
                    changed.push(receipt(&binding));
                }
            }
            finalize_scope(tx, key)?;
            Ok(changed)
        })
        .map_err(RepositoryError::pap)
    }
    fn sync_scope_instances(
        &self,
        key: &ResourceId,
        instances: &[ProcessIdentity],
    ) -> Result<Vec<BindingIntentReceipt>, PapError> {
        self.access(true, |tx| {
            let scope = read_scope(tx, key)?.ok_or(PapError::NotFound)?;
            if scope.scope.status != ScopeStatus::Active {
                return Err(PapError::OperationInProgress.into());
            }
            if let ScopeSelector::Pid { pid } = scope.scope.selector {
                if instances.len() > 1 || instances.first().is_some_and(|p| p.pid != pid) {
                    return Err(PapError::Conflict.into());
                }
                if let Some(instance) = instances.first() {
                    if scope.pin.as_ref().is_some_and(|pin| pin != instance) {
                        return Err(PapError::Conflict.into());
                    }
                    if scope.pin.is_none() {
                        tx.execute(
                            "UPDATE scopes SET pinned_process_json=?2 WHERE scope_id=?1",
                            params![key.as_str(), encode(instance)?],
                        )?;
                    }
                }
            }
            let mut changed = Vec::new();
            let mut known = BTreeSet::new();
            for binding_id in binding_ids(tx, key)? {
                let mut binding = read_binding(tx, &binding_id)?.ok_or(StoreError::Corrupt)?;
                known.insert((
                    binding.binding.spec.policy.policy_id.clone(),
                    binding.binding.spec.scope.process.clone(),
                ));
                if !instances.contains(&binding.binding.spec.scope.process) && retire(&mut binding)?
                {
                    save_binding(tx, &binding)?;
                    changed.push(receipt(&binding));
                }
            }
            for instance in instances {
                for policy in &scope.scope.policy_snapshots {
                    if !known.insert((policy.policy_id.clone(), instance.clone())) {
                        continue;
                    }
                    let snapshot = BindingStateSnapshot {
                        binding: BindingView {
                            spec: PreparedBinding {
                                binding_id: id(uuid::Uuid::new_v4().to_string())?,
                                binding_revision: Revision::new(1)
                                    .map_err(|_| StoreError::Invalid)?,
                                policy: policy.clone(),
                                scope: BindingScope {
                                    scope_id: key.clone(),
                                    selector: scope.scope.selector.clone(),
                                    process: instance.clone(),
                                },
                            },
                            status: BindingStatus::PendingApply.into(),
                        },
                        status_version: 1,
                        deployments: Vec::new(),
                    };
                    snapshot
                        .binding
                        .validate()
                        .map_err(PapError::InvalidBinding)?;
                    save_binding(tx, &snapshot)?;
                    changed.push(receipt(&snapshot));
                }
            }
            Ok(changed)
        })
        .map_err(RepositoryError::pap)
    }
    fn retry_scope(&self, key: &ResourceId) -> Result<Vec<BindingIntentReceipt>, PapError> {
        self.access(true, |tx| {
            read_scope(tx, key)?.ok_or(PapError::NotFound)?;
            let mut changed = Vec::new();
            for key in binding_ids(tx, key)? {
                let mut binding = read_binding(tx, &key)?.ok_or(StoreError::Corrupt)?;
                let phase = match binding.binding.status.phase {
                    BindingStatus::ApplyFailed => BindingStatus::PendingApply,
                    BindingStatus::DeleteFailed => BindingStatus::PendingDelete,
                    _ => continue,
                };
                change_status(&mut binding, phase.into())?;
                save_binding(tx, &binding)?;
                changed.push(receipt(&binding));
            }
            Ok(changed)
        })
        .map_err(RepositoryError::pap)
    }
    fn fail_pending_binding(
        &self,
        expected: &BindingIntentReceipt,
        reason: asc_pap::EnqueueError,
    ) -> Result<bool, PapError> {
        self.access(true, |tx| {
            let phase = match expected.binding.status.phase {
                BindingStatus::PendingApply => BindingStatus::ApplyFailed,
                BindingStatus::PendingDelete => BindingStatus::DeleteFailed,
                _ => return Err(PapError::Conflict.into()),
            };
            let Some(mut current) = read_binding(tx, &expected.binding.spec.binding_id)? else {
                return Ok(false);
            };
            if receipt(&current) != *expected {
                return Ok(false);
            }
            let mut status: asc_policy_types::binding::BindingLifecycle = phase.into();
            status.error = Some(reason.failure());
            change_status(&mut current, status)?;
            save_binding(tx, &current)?;
            Ok(true)
        })
        .map_err(RepositoryError::pap)
    }
    fn get_binding(&self, key: &ResourceId) -> Result<BindingView, PapError> {
        self.access(false, |tx| {
            Ok(read_binding(tx, key)?.ok_or(PapError::NotFound)?.binding)
        })
        .map_err(RepositoryError::pap)
    }
    fn list_bindings(&self, limit: u32, offset: u32) -> Result<Page<BindingView>, PapError> {
        self.access(false, |tx| {
            let total = tx.query_row("SELECT count(*) FROM bindings", [], |r| r.get(0))?;
            let keys: Vec<String> = tx
                .prepare("SELECT binding_id FROM bindings ORDER BY binding_id LIMIT ?1 OFFSET ?2")?
                .query_map(params![limit, offset], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            page(
                total,
                keys.into_iter().map(|key| {
                    Ok(read_binding(tx, &id(key)?)?
                        .ok_or(StoreError::Corrupt)?
                        .binding)
                }),
            )
        })
        .map_err(RepositoryError::pap)
    }
}

fn retire(binding: &mut BindingStateSnapshot) -> Result<bool, RepositoryError> {
    if matches!(
        binding.binding.status.phase,
        BindingStatus::PendingDelete
            | BindingStatus::Deleting
            | BindingStatus::DeleteFailed
            | BindingStatus::Deleted
    ) {
        return Ok(false);
    }
    change_status(binding, BindingStatus::PendingDelete.into())?;
    Ok(true)
}
