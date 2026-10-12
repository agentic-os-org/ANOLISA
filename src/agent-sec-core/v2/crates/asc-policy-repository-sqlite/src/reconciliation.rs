//! Atomic reconcile writes and bounded catalog reads.
use crate::{
    RepositoryError, SqlitePolicyRepository,
    records::{decode, encode, finalize_scope, id, read_binding, save_binding},
};
use asc_foundation_types::ResourceId;
use asc_policy_repository::{
    BindingReconcileCatalog, BindingStateRepository, BindingStateSnapshot, BindingStateWrite,
    ReconcileCandidate, StoreError, WriteReceipt, WriteResult, apply_binding_write, write_digest,
};
use rusqlite::{OptionalExtension, params};

impl BindingStateRepository for SqlitePolicyRepository {
    fn check_writable(&self) -> Result<(), StoreError> {
        self.check_storage().map_err(RepositoryError::store)
    }
    fn get_binding_state(
        &self,
        id: &ResourceId,
    ) -> Result<Option<BindingStateSnapshot>, StoreError> {
        self.access(false, |tx| read_binding(tx, id))
            .map_err(RepositoryError::store)
    }
    #[tracing::instrument(skip_all, name = "policy.repository.cas", fields(
        binding_id = %expected.binding.spec.binding_id,
        scope_id = %expected.binding.spec.scope.scope_id,
        expected_version = expected.status_version,
    ))]
    fn compare_exchange_binding_state(
        &self,
        expected: &BindingStateSnapshot,
        write: &BindingStateWrite,
    ) -> Result<WriteResult, StoreError> {
        let result = self
            .access(true, |tx| {
                let key = &expected.binding.spec.binding_id;
                let digest = write_digest(expected, write)?;
                let previous: Option<(Vec<u8>, String)> = tx
                    .query_row(
                        "SELECT last_write_digest,last_write_result_json FROM bindings
                 WHERE binding_id=?1 AND last_write_id=?2",
                        params![key.as_str(), write.write_id.to_string()],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                if let Some((previous_digest, result)) = previous {
                    if previous_digest != digest {
                        return Err(StoreError::Invalid.into());
                    }
                    return Ok(WriteResult::AlreadyApplied(decode(&result)?));
                }
                let Some(current) = read_binding(tx, key)? else {
                    return Ok(if write.next.is_none() {
                        WriteResult::AlreadyApplied(WriteReceipt {
                            status_version: None,
                            status_applied: true,
                        })
                    } else {
                        WriteResult::Conflict
                    });
                };
                let (next, result) = apply_binding_write(&current, expected, write)?;
                let WriteResult::Applied(receipt) = result else {
                    return Ok(result);
                };
                if let Some(next) = next {
                    save_binding(tx, &next)?;
                    tx.execute(
                        "UPDATE bindings SET last_write_id=?2,last_write_digest=?3,
                     last_write_result_json=?4 WHERE binding_id=?1",
                        params![
                            key.as_str(),
                            write.write_id.to_string(),
                            digest.as_slice(),
                            encode(&receipt)?
                        ],
                    )?;
                } else {
                    tx.execute("DELETE FROM bindings WHERE binding_id=?1", [key.as_str()])?;
                    finalize_scope(tx, &current.binding.spec.scope.scope_id)?;
                }
                Ok(result)
            })
            .map_err(RepositoryError::store);
        tracing::debug!(
            target: "asc_observability::diagnostic",
            component = "policy_repository",
            binding_id = %expected.binding.spec.binding_id,
            scope_id = %expected.binding.spec.scope.scope_id,
            write_id = %write.write_id,
            expected_version = expected.status_version,
            expected_phase = ?expected.binding.status.phase,
            next_phase = ?write.next.as_ref().and_then(|patch| patch.status.as_ref().map(|status| status.phase)),
            deleting_record = write.next.is_none(),
            deployments = ?write.next.as_ref().and_then(|patch| patch.deployments.as_ref())
                .map(|deployments| deployments.iter()
                    .map(|deployment| (&deployment.target.route, &deployment.target.id, deployment.presence))
                    .collect::<Vec<_>>()),
            result = ?result,
            "binding CAS completed"
        );
        result
    }
}
impl BindingReconcileCatalog for SqlitePolicyRepository {
    fn scan_reconciliation(
        &self,
        after: Option<&ResourceId>,
        limit: usize,
    ) -> Result<Vec<ReconcileCandidate>, StoreError> {
        if limit == 0 || limit > 1000 {
            return Err(StoreError::Invalid);
        }
        self.access(false, |tx| {
            let rows: Vec<(String, String)> = tx
                .prepare(
                    "SELECT binding_id,phase FROM bindings WHERE (?1 IS NULL OR binding_id>?1)
                 ORDER BY binding_id LIMIT ?2",
                )?
                .query_map(params![after.map(ResourceId::as_str), limit], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?
                .collect::<Result<_, _>>()?;
            rows.into_iter()
                .map(|(key, phase)| {
                    Ok(ReconcileCandidate {
                        id: id(key)?,
                        status: decode(&format!("\"{phase}\""))?,
                    })
                })
                .collect()
        })
        .map_err(RepositoryError::store)
    }
}
