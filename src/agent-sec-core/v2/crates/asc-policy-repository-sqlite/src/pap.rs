//! PAP transactions; notifications and discovery threads run after commit.
use crate::{
    RepositoryError, SqlitePolicyRepository,
    records::{
        binding_ids, change_status, encode, finalize_scope, id, insert_scope, page, read_binding,
        read_policy, read_scope, receipt, save_binding,
    },
};
use asc_foundation_types::{ResourceId, Revision};
use asc_pap::{
    Page, PapError, PapRepository, PolicyRevisionState, PolicySnapshot, PolicySnapshotKind,
    ScopeDiscoverySeed,
};
use asc_policy_repository::{BindingIntentReceipt, BindingStateSnapshot, StoreError};
use asc_policy_types::{
    Validate,
    binding::{BindingScope, BindingStatus, BindingView, PreparedBinding},
    policy::PreparedPolicy,
    process_discovery::ProcessIdentity,
    scope::{PreparedScope, ScopeSelector, ScopeStatus},
};
use rusqlite::{OptionalExtension, params};
use std::collections::BTreeSet;

const AUTOMATIC_RECORD_LIMIT: usize = 4096;
const AUTOMATIC_BYTE_LIMIT: usize = 16 * 1024 * 1024;
const AUTOMATIC_SOURCE_FILTER: &str = "json_extract(current_json,'$.template.language')='cedar'
    AND json_extract(current_json,'$.template.content.schema.schemaId')=?1
    AND json_extract(current_json,'$.template.content.schema.revision')=?2";

impl PapRepository for SqlitePolicyRepository {
    fn put_policy(&self, policy: &PreparedPolicy) -> Result<PreparedPolicy, PapError> {
        if policy.deployment.is_some() {
            return Err(PapError::InvalidPolicy(
                asc_policy_types::ValidationError::new(
                    "deployment",
                    "authored Policies cannot contain deployment selections",
                ),
            ));
        }
        asc_pap::validate_policy_snapshot(policy)?;
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
            if asc_pap::is_automatic_source(policy) {
                check_automatic_capacity(tx, Some((&policy.policy_id, encoded.len())))?;
            }
            tx.execute(
                "INSERT INTO policies(policy_id,last_allocated_revision,current_json)
                 VALUES (?1,?2,?3) ON CONFLICT(policy_id) DO UPDATE SET
                 last_allocated_revision=excluded.last_allocated_revision,
                 current_json=excluded.current_json",
                params![policy.policy_id.as_str(), policy.revision.get(), encoded],
            )?;
            advance_policy_generation(tx)?;
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
    fn current_policies(&self, ids: &[ResourceId]) -> Result<Vec<PreparedPolicy>, PapError> {
        if ids.is_empty() || ids.len() > 32 {
            return Err(PapError::InvalidPolicy(
                asc_policy_types::error::ValidationError::new("policyIds", "requires 1..32 IDs"),
            ));
        }
        self.access(false, |tx| {
            ids.iter()
                .map(|id| {
                    read_policy(tx, id)?
                        .and_then(|state| state.current)
                        .ok_or_else(|| PapError::NotFound.into())
                })
                .collect()
        })
        .map_err(RepositoryError::pap)
    }
    fn policy_generation(&self) -> Result<u64, PapError> {
        self.access(false, read_policy_generation)
            .map_err(RepositoryError::pap)
    }
    fn policy_snapshot(&self, kind: PolicySnapshotKind) -> Result<PolicySnapshot, PapError> {
        self.access(false, |tx| {
            let generation = read_policy_generation(tx)?;
            let policies = match kind {
                PolicySnapshotKind::AllCurrent => all_current_policies(tx)?,
                PolicySnapshotKind::Automatic => automatic_policies(tx)?,
            };
            Ok(PolicySnapshot {
                generation,
                policies,
            })
        })
        .map_err(RepositoryError::pap)
    }
    fn automatic_source_revisions(&self) -> Result<Vec<(ResourceId, Revision)>, PapError> {
        self.access(false, automatic_sources)
            .map_err(RepositoryError::pap)
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
            advance_policy_generation(tx)?;
            Ok(current)
        })
        .map_err(RepositoryError::pap)
    }
    fn put_scope(&self, scope: &PreparedScope) -> Result<PreparedScope, PapError> {
        scope.validate().map_err(PapError::InvalidScope)?;
        self.access(true, |tx| {
            if encode(scope)?.len() > asc_pap::MAX_SCOPE_BYTES {
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
            let count: usize = tx.query_row(
                "SELECT count(*) FROM scopes WHERE phase='ACTIVE'",
                [],
                |r| r.get(0),
            )?;
            if count >= asc_pap::MAX_ACTIVE_SCOPES {
                return Err(PapError::Unavailable.into());
            }
            insert_scope(tx, scope)?;
            #[cfg(feature = "fault-injection")]
            fail::fail_point!("policy.sqlite.scope.inserted");
            Ok(scope.clone())
        })
        .map_err(RepositoryError::pap)
    }
    fn validate_automatic_scopes(
        &self,
        updates: &asc_pap::ScopePublication,
        sources: &[(ResourceId, Revision)],
    ) -> Result<(), PapError> {
        self.transaction(crate::TransactionMode::Preview, |tx| {
            apply_automatic_scopes(tx, updates, sources)
        })
        .map_err(RepositoryError::pap)
    }
    fn replace_automatic_scopes(
        &self,
        updates: &asc_pap::ScopePublication,
        sources: &[(ResourceId, Revision)],
    ) -> Result<(), PapError> {
        self.access(true, |tx| apply_automatic_scopes(tx, updates, sources))
            .map_err(RepositoryError::pap)
    }
    fn delete_automatic_scope(
        &self,
        expected: &PreparedScope,
        sources: &[(ResourceId, Revision)],
    ) -> Result<PreparedScope, PapError> {
        self.access(true, |tx| {
            check_sources(tx, sources)?;
            let mut scope = read_scope(tx, &expected.scope_id)?
                .ok_or(PapError::NotFound)?
                .scope;
            if &scope != expected || scope.automatic_backend().is_none() {
                return Err(PapError::Conflict.into());
            }
            scope.status = ScopeStatus::Deleting;
            tx.execute(
                "UPDATE scopes SET phase='DELETING' WHERE scope_id=?1",
                [scope.scope_id.as_str()],
            )?;
            Ok(scope)
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
            #[cfg(feature = "fault-injection")]
            fail::fail_point!("policy.sqlite.discovery.stopped");
            let mut changed = Vec::new();
            for key in binding_ids(tx, key)? {
                let mut binding = read_binding(tx, &key)?.ok_or(StoreError::Corrupt)?;
                if retire(&mut binding)? {
                    save_binding(tx, &binding)?;
                    #[cfg(feature = "fault-injection")]
                    fail::fail_point!("policy.sqlite.discovery.binding_retired");
                    changed.push(receipt(&binding));
                }
            }
            finalize_scope(tx, key)?;
            Ok(changed)
        })
        .map_err(RepositoryError::pap)
    }
    fn scope_execution_ready_to_release(&self, key: &ResourceId) -> Result<bool, PapError> {
        self.access(false, |tx| {
            let Some(scope) = read_scope(tx, key)? else {
                return Ok(false);
            };
            Ok(scope.execution_pending
                && scope.scope.status == ScopeStatus::Deleting
                && scope.discovery_stopped
                && binding_ids(tx, key)?.is_empty())
        })
        .map_err(RepositoryError::pap)
    }
    fn finish_scope_execution(&self, key: &ResourceId) -> Result<(), PapError> {
        self.access(true, |tx| crate::records::confirm_scope_execution(tx, key))
            .map_err(RepositoryError::pap)
    }
    fn sync_scope_instances(
        &self,
        key: &ResourceId,
        instances: &[ProcessIdentity],
    ) -> Result<Vec<BindingIntentReceipt>, PapError> {
        self.sync_observed_scope_instances(key, instances, &std::collections::BTreeMap::new(), None)
    }
    fn automatic_scope_generation(&self, key: &ResourceId) -> Result<Option<u64>, PapError> {
        self.access(false, |tx|{
            let generation=tx.query_row("SELECT json_extract(assignment_json,'$.generation') FROM scopes WHERE scope_id=?1 AND phase='ACTIVE'",[key.as_str()],|row|row.get(0))
                .optional()?.ok_or(PapError::NotFound)?;
            Ok(generation)
        }).map_err(RepositoryError::pap)
    }
    fn sync_observed_scope_instances(
        &self,
        key: &ResourceId,
        instances: &[ProcessIdentity],
        uids: &std::collections::BTreeMap<ProcessIdentity, u32>,
        generation: Option<u64>,
    ) -> Result<Vec<BindingIntentReceipt>, PapError> {
        self.access(true, |tx| {
            let scope = read_scope(tx, key)?.ok_or(PapError::NotFound)?;
            if scope.scope.status != ScopeStatus::Active {
                return Err(PapError::OperationInProgress.into());
            }
            if scope.generation != generation {
                return Err(PapError::Conflict.into());
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
                        #[cfg(feature = "fault-injection")]
                        fail::fail_point!("policy.sqlite.instances.pinned");
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
                let changed_binding = reconcile_existing(&mut binding, &scope.scope, instances)?;
                if changed_binding {
                    save_binding(tx, &binding)?;
                    changed.push(receipt(&binding));
                }
            }
            for instance in instances {
                if scope.scope.automatic_backend().is_some()
                    && scope
                        .scope
                        .owner_uid
                        .is_some_and(|uid| uids.get(instance) != Some(&uid))
                {
                    continue;
                }
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
                    #[cfg(feature = "fault-injection")]
                    fail::fail_point!("policy.sqlite.instances.binding_inserted");
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
            Ok(public_binding(
                read_binding(tx, key)?.ok_or(PapError::NotFound)?.binding,
            ))
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
                    Ok(public_binding(
                        read_binding(tx, &id(key)?)?
                            .ok_or(StoreError::Corrupt)?
                            .binding,
                    ))
                }),
            )
        })
        .map_err(RepositoryError::pap)
    }
}

pub(crate) fn read_policy_generation(
    tx: &rusqlite::Transaction<'_>,
) -> Result<u64, RepositoryError> {
    tx.query_row(
        "SELECT generation FROM policy_generation WHERE singleton=1",
        [],
        |row| row.get(0),
    )
    .optional()?
    .ok_or(StoreError::Corrupt.into())
}

fn advance_policy_generation(tx: &rusqlite::Transaction<'_>) -> Result<(), RepositoryError> {
    if tx.execute(
        "UPDATE policy_generation SET generation=generation+1 WHERE singleton=1",
        [],
    )? != 1
    {
        return Err(StoreError::Corrupt.into());
    }
    Ok(())
}

fn all_current_policies(
    tx: &rusqlite::Transaction<'_>,
) -> Result<Vec<PreparedPolicy>, RepositoryError> {
    let mut statement = tx.prepare(
        "SELECT policy_id FROM policies WHERE current_json IS NOT NULL ORDER BY policy_id",
    )?;
    let keys = statement.query_map([], |row| row.get::<_, String>(0))?;
    let mut policies = Vec::new();
    let mut bytes = 0;
    for key in keys {
        let policy = read_policy(tx, &id(key?)?)?
            .and_then(|state| state.current)
            .ok_or(StoreError::Corrupt)?;
        bytes += encode(&policy)?.len();
        // Bound one in-process snapshot, failing closed rather than dropping members.
        if policies.len() >= 4096 || bytes > 16 * 1024 * 1024 {
            return Err(
                PapError::InvalidPolicy(asc_policy_types::error::ValidationError::new(
                    "authorizationPolicies",
                    "current policy snapshot exceeds 4096 records or 16 MiB",
                ))
                .into(),
            );
        }
        policies.push(policy);
    }
    Ok(policies)
}
fn automatic_policies(
    tx: &rusqlite::Transaction<'_>,
) -> Result<Vec<PreparedPolicy>, RepositoryError> {
    check_automatic_capacity(tx, None)?;
    automatic_sources(tx)?
        .into_iter()
        .map(|(key, _)| {
            read_policy(tx, &key)?
                .and_then(|state| state.current)
                .ok_or(StoreError::Corrupt.into())
        })
        .collect()
}

fn public_binding(mut binding: BindingView) -> BindingView {
    binding.status.error = binding
        .status
        .error
        .as_ref()
        .map(|error| asc_policy_types::target::Failure::new(error.kind, &error.code));
    binding
}

// Preview and commit exercise the same SQL constraints and complete source CAS.
fn apply_automatic_scopes(
    tx: &rusqlite::Transaction<'_>,
    updates: &asc_pap::ScopePublication,
    sources: &[(ResourceId, Revision)],
) -> Result<(), RepositoryError> {
    check_sources(tx, sources)?;
    let mut added = false;
    for (id, update) in updates {
        let current = read_scope(tx, id)?;
        if current.as_ref().map(|row| &row.scope) != update.expected.as_ref()
            || update.expected.as_ref().is_some_and(|scope| {
                scope.status != ScopeStatus::Active || scope.automatic_backend().is_none()
            })
        {
            return Err(PapError::Conflict.into());
        }
        let Some(candidate) = &update.candidate else {
            if current.is_none() {
                return Err(PapError::Conflict.into());
            }
            tx.execute(
                "UPDATE scopes SET phase='DELETING' WHERE scope_id=?1",
                [id.as_str()],
            )?;
            continue;
        };
        candidate.validate().map_err(PapError::InvalidScope)?;
        if &candidate.scope_id != id
            || candidate.automatic_backend().is_none()
            || candidate.status != ScopeStatus::Active
            || update.expected.as_ref().is_some_and(|old| {
                old.selector != candidate.selector
                    || old.owner_uid != candidate.owner_uid
                    || old.automatic_backend() != candidate.automatic_backend()
            })
        {
            return Err(PapError::Conflict.into());
        }
        if encode(candidate)?.len() > asc_pap::MAX_SCOPE_BYTES {
            return Err(
                PapError::InvalidScope(asc_policy_types::ValidationError::new(
                    "policyTemplates",
                    "serialized assignment exceeds 1 MiB",
                ))
                .into(),
            );
        }
        check_originals(tx, candidate)?;
        if let Some(row) = current {
            if row.scope != *candidate {
                let generation = row
                    .generation
                    .and_then(|n| n.checked_add(1))
                    .ok_or(StoreError::Corrupt)?;
                crate::records::replace_scope_assignment(tx, candidate, generation)?;
            }
        } else {
            insert_scope(tx, candidate)?;
            added = true;
        }
    }
    if added {
        // Check the final membership so the order of source and destination IDs is immaterial.
        let active: usize = tx.query_row(
            "SELECT count(*) FROM scopes WHERE phase='ACTIVE'",
            [],
            |r| r.get(0),
        )?;
        if active > asc_pap::MAX_ACTIVE_SCOPES {
            return Err(
                PapError::InvalidScope(asc_policy_types::ValidationError::new(
                    "scopes",
                    format!(
                        "exceeds the limit of {} active Scopes",
                        asc_pap::MAX_ACTIVE_SCOPES
                    ),
                ))
                .into(),
            );
        }
    }
    Ok(())
}
fn check_originals(
    tx: &rusqlite::Transaction<'_>,
    candidate: &PreparedScope,
) -> Result<(), RepositoryError> {
    for projected in &candidate.policy_snapshots {
        if !asc_pap::is_automatic_source(projected) {
            return Err(PapError::Conflict.into());
        }
        let mut original = projected.clone();
        original.deployment = None;
        if read_policy(tx, &original.policy_id)?
            .and_then(|state| state.current)
            .as_ref()
            != Some(&original)
        {
            return Err(PapError::ReferencedPolicyRevisionNotFound.into());
        }
    }
    Ok(())
}

fn check_sources(
    tx: &rusqlite::Transaction<'_>,
    expected: &[(ResourceId, Revision)],
) -> Result<(), RepositoryError> {
    if automatic_sources(tx)? != expected {
        return Err(PapError::Conflict.into());
    }
    Ok(())
}

// Measure the exact stored UTF-8 bytes. Replacing an existing v3 source with a
// smaller one must remain possible even if an older database is already over quota.
fn check_automatic_capacity(
    tx: &rusqlite::Transaction<'_>,
    replacement: Option<(&ResourceId, usize)>,
) -> Result<(), RepositoryError> {
    let replacing = replacement.map(|(key, _)| key.as_str());
    let (count, bytes, old): (usize, usize, usize) = tx.query_row(
        &format!(
            "SELECT count(*), coalesce(sum(size),0), coalesce(max(CASE WHEN policy_id=?3 THEN size END),0)
             FROM (SELECT policy_id, length(CAST(current_json AS BLOB)) AS size FROM policies
                   WHERE {AUTOMATIC_SOURCE_FILTER})"
        ),
        params![asc_pap::SCHEMA_ID, asc_pap::PRINCIPAL_SCHEMA_REVISION, replacing],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let (count, bytes) = match replacement {
        Some((_, size)) if size < old => return Ok(()),
        Some((_, size)) => (count + usize::from(old == 0), bytes - old + size),
        None => (count, bytes),
    };
    if count > AUTOMATIC_RECORD_LIMIT || bytes > AUTOMATIC_BYTE_LIMIT {
        return Err(PapError::AutomaticPolicyCapacity.into());
    }
    Ok(())
}

fn automatic_sources(
    tx: &rusqlite::Transaction<'_>,
) -> Result<Vec<(ResourceId, Revision)>, RepositoryError> {
    let mut stmt = tx.prepare(&format!(
        "SELECT policy_id,last_allocated_revision FROM policies WHERE {AUTOMATIC_SOURCE_FILTER}
         ORDER BY policy_id LIMIT ?3"
    ))?;
    let mut sources = Vec::new();
    for row in stmt.query_map(
        params![
            asc_pap::SCHEMA_ID,
            asc_pap::PRINCIPAL_SCHEMA_REVISION,
            AUTOMATIC_RECORD_LIMIT + 1
        ],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?)),
    )? {
        let (key, revision) = row?;
        sources.push((
            id(key)?,
            Revision::new(revision).map_err(|_| StoreError::Corrupt)?,
        ));
    }
    if sources.len() > AUTOMATIC_RECORD_LIMIT {
        return Err(PapError::AutomaticPolicyCapacity.into());
    }
    Ok(sources)
}

fn reconcile_existing(
    binding: &mut BindingStateSnapshot,
    scope: &PreparedScope,
    instances: &[ProcessIdentity],
) -> Result<bool, RepositoryError> {
    let Some(policy) = scope
        .policy_snapshots
        .iter()
        .find(|policy| policy.policy_id == binding.binding.spec.policy.policy_id)
        .filter(|_| instances.contains(&binding.binding.spec.scope.process))
    else {
        return retire(binding);
    };
    let Some(backend) = scope.automatic_backend() else {
        return Ok(false);
    };
    if matches!(
        binding.binding.status.phase,
        BindingStatus::PendingDelete
            | BindingStatus::Deleting
            | BindingStatus::DeleteFailed
            | BindingStatus::Deleted
    ) {
        return Ok(false);
    }
    if &binding.binding.spec.policy != policy {
        binding.binding.spec.binding_revision = binding
            .binding
            .spec
            .binding_revision
            .checked_next()
            .map_err(|_| PapError::RevisionExhausted)?;
        binding.binding.spec.policy = policy.clone();
    } else if !(backend == asc_policy_types::policy::DeploymentBackend::ActPlane
        && binding.binding.status.phase == BindingStatus::ApplyFailed
        && binding
            .binding
            .status
            .error
            .as_ref()
            .is_some_and(|failure| failure.code == "AGENTSIGHT_BINDING_CONFLICT"))
    {
        return Ok(false);
    }
    change_status(binding, BindingStatus::PendingApply.into())?;
    Ok(true)
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
