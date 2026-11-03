//! Version-one generic policy snapshots and projection checks for every SQL read path.
use crate::RepositoryError;
use asc_foundation_types::{ResourceId, Revision};
use asc_pap::{Page, PapError};
use asc_policy_repository::{BindingIntentReceipt, BindingStateSnapshot, StoreError, WriteReceipt};
use asc_policy_types::{
    Validate,
    binding::{BindingLifecycle, BindingStatus, BindingView, PreparedBinding},
    policy::PreparedPolicy,
    process_discovery::ProcessIdentity,
    scope::{PreparedScope, ScopeSelector, ScopeStatus},
};
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub(crate) fn encode<T: Serialize + ?Sized>(value: &T) -> Result<String, RepositoryError> {
    serde_json::to_string(value).map_err(|_| StoreError::Invalid.into())
}
pub(crate) fn decode<T: DeserializeOwned>(value: &str) -> Result<T, RepositoryError> {
    serde_json::from_str(value).map_err(|_| StoreError::Corrupt.into())
}
pub(crate) fn phase(phase: BindingStatus) -> Result<String, RepositoryError> {
    Ok(encode(&phase)?.trim_matches('"').to_owned())
}
pub(crate) fn id(value: String) -> Result<ResourceId, RepositoryError> {
    ResourceId::new(value).map_err(|_| StoreError::Corrupt.into())
}
pub(crate) fn receipt(snapshot: &BindingStateSnapshot) -> BindingIntentReceipt {
    snapshot.into()
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Assignment {
    selector: ScopeSelector,
    policy_snapshots: Vec<PreparedPolicy>,
}
pub(crate) struct ScopeRow {
    pub scope: PreparedScope,
    pub pin: Option<ProcessIdentity>,
}

pub(crate) fn read_scope(
    tx: &Transaction<'_>,
    key: &ResourceId,
) -> Result<Option<ScopeRow>, RepositoryError> {
    let row: Option<(String,String,Option<String>,bool)> = tx.query_row(
        "SELECT assignment_json,phase,pinned_process_json,discovery_stopped FROM scopes WHERE scope_id=?1", [key.as_str()],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    row.map(|(assignment, phase, pin, stopped)| {
        let assignment: Assignment = decode(&assignment)?;
        let status = match phase.as_str() {
            "ACTIVE" => ScopeStatus::Active,
            "DELETING" => ScopeStatus::Deleting,
            _ => return Err(StoreError::Corrupt.into()),
        };
        let scope = PreparedScope {
            scope_id: key.clone(),
            selector: assignment.selector,
            policy_snapshots: assignment.policy_snapshots,
            status,
        };
        scope.validate().map_err(|_| StoreError::Corrupt)?;
        let pin: Option<ProcessIdentity> = pin.map(|p| decode(&p)).transpose()?;
        if let Some(pin) = &pin {
            asc_policy_types::binding::BindingScope {
                scope_id: scope.scope_id.clone(),
                selector: scope.selector.clone(),
                process: pin.clone(),
            }
            .validate()
            .map_err(|_| StoreError::Corrupt)?;
            if !matches!(scope.selector,ScopeSelector::Pid{pid} if pid==pin.pid) {
                return Err(StoreError::Corrupt.into());
            }
        }
        if stopped && status != ScopeStatus::Deleting {
            return Err(StoreError::Corrupt.into());
        }
        Ok(ScopeRow { scope, pin })
    })
    .transpose()
}

pub(crate) fn insert_scope(
    tx: &Transaction<'_>,
    scope: &PreparedScope,
) -> Result<(), RepositoryError> {
    let assignment = Assignment {
        selector: scope.selector.clone(),
        policy_snapshots: scope.policy_snapshots.clone(),
    };
    tx.execute(
        "INSERT INTO scopes(scope_id,assignment_json,phase) VALUES (?1,?2,'ACTIVE')",
        params![scope.scope_id.as_str(), encode(&assignment)?],
    )?;
    Ok(())
}

pub(crate) fn finalize_scope(
    tx: &Transaction<'_>,
    key: &ResourceId,
) -> Result<(), RepositoryError> {
    tx.execute(
        "DELETE FROM scopes WHERE scope_id=?1 AND phase='DELETING'
         AND discovery_stopped=1 AND NOT EXISTS (SELECT 1 FROM bindings WHERE scope_id=?1)",
        [key.as_str()],
    )?;
    Ok(())
}

pub(crate) fn read_policy(
    tx: &Transaction<'_>,
    key: &ResourceId,
) -> Result<Option<asc_pap::PolicyRevisionState>, RepositoryError> {
    let row: Option<(u32, Option<String>)> = tx
        .query_row(
            "SELECT last_allocated_revision,current_json FROM policies WHERE policy_id=?1",
            [key.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    row.map(|(head, current)| {
        let last_allocated_revision = Revision::new(head).map_err(|_| StoreError::Corrupt)?;
        let current: Option<PreparedPolicy> = current.map(|v| decode(&v)).transpose()?;
        if let Some(policy) = &current {
            policy.validate().map_err(|_| StoreError::Corrupt)?;
            if policy.policy_id != *key || policy.revision != last_allocated_revision {
                return Err(StoreError::Corrupt.into());
            }
        }
        Ok(asc_pap::PolicyRevisionState {
            last_allocated_revision,
            current,
        })
    })
    .transpose()
}

pub(crate) fn read_binding(
    tx: &Transaction<'_>,
    key: &ResourceId,
) -> Result<Option<BindingStateSnapshot>, RepositoryError> {
    let mut query = tx.prepare(
        "SELECT spec_json,phase,error_json,status_version,deployments_json,scope_id,
         policy_id,boot_id,pid_namespace,pid,start_time,binding_revision
         FROM bindings WHERE binding_id=?1",
    )?;
    let mut rows = query.query([key.as_str()])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let spec: PreparedBinding = decode(&row.get::<_, String>(0)?)?;
    let status = BindingLifecycle {
        phase: decode(&format!("\"{}\"", row.get::<_, String>(1)?))?,
        error: row
            .get::<_, Option<String>>(2)?
            .map(|v| decode(&v))
            .transpose()?,
    };
    let snapshot = BindingStateSnapshot {
        binding: BindingView { spec, status },
        status_version: row.get(3)?,
        deployments: decode(&row.get::<_, String>(4)?)?,
    };
    snapshot
        .binding
        .validate()
        .map_err(|_| StoreError::Corrupt)?;
    let spec = &snapshot.binding.spec;
    let process = &spec.scope.process;
    if spec.binding_id != *key
        || snapshot.status_version < 1
        || spec.scope.scope_id.as_str() != row.get::<_, String>(5)?
        || spec.policy.policy_id.as_str() != row.get::<_, String>(6)?
        || process.boot_id != row.get::<_, String>(7)?
        || process.pid_namespace != row.get::<_, String>(8)?
        || process.pid != row.get::<_, u32>(9)?
        || process.start_time.to_string() != row.get::<_, String>(10)?
        || spec.binding_revision.get() != row.get::<_, u32>(11)?
    {
        return Err(StoreError::Corrupt.into());
    }
    if snapshot.deployments.len() > 32 || encode(&snapshot.deployments)?.len() > 1024 * 1024 {
        return Err(StoreError::Corrupt.into());
    }
    for (i, d) in snapshot.deployments.iter().enumerate() {
        if d.presence == asc_policy_types::target::Presence::Absent
            || snapshot.deployments[..i]
                .iter()
                .any(|p| p.target.same_identity(&d.target))
        {
            return Err(StoreError::Corrupt.into());
        }
    }
    Ok(Some(snapshot))
}

pub(crate) fn save_binding(
    tx: &Transaction<'_>,
    snapshot: &BindingStateSnapshot,
) -> Result<(), RepositoryError> {
    let b = &snapshot.binding;
    let s = &b.spec;
    let p = &s.scope.process;
    tx.execute(
        "INSERT INTO bindings(binding_id,scope_id,policy_id,boot_id,pid_namespace,pid,start_time,
         binding_revision,spec_json,phase,error_json,status_version,deployments_json)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)
         ON CONFLICT(binding_id) DO UPDATE SET
         scope_id=excluded.scope_id,policy_id=excluded.policy_id,boot_id=excluded.boot_id,
         pid_namespace=excluded.pid_namespace,pid=excluded.pid,start_time=excluded.start_time,
         binding_revision=excluded.binding_revision,spec_json=excluded.spec_json,
         phase=excluded.phase,error_json=excluded.error_json,
         status_version=excluded.status_version,deployments_json=excluded.deployments_json",
        params![
            s.binding_id.as_str(),
            s.scope.scope_id.as_str(),
            s.policy.policy_id.as_str(),
            p.boot_id,
            p.pid_namespace,
            p.pid,
            p.start_time.to_string(),
            s.binding_revision.get(),
            encode(s)?,
            phase(b.status.phase)?,
            b.status.error.as_ref().map(encode).transpose()?,
            snapshot.status_version,
            encode(&snapshot.deployments)?,
        ],
    )?;
    Ok(())
}

pub(crate) fn change_status(
    snapshot: &mut BindingStateSnapshot,
    status: BindingLifecycle,
) -> Result<(), RepositoryError> {
    if snapshot.binding.status != status {
        snapshot.status_version = snapshot
            .status_version
            .checked_add(1)
            .ok_or(StoreError::Invalid)?;
        snapshot.binding.status = status;
    }
    Ok(())
}

pub(crate) fn binding_ids(
    tx: &Transaction<'_>,
    scope: &ResourceId,
) -> Result<Vec<ResourceId>, RepositoryError> {
    let keys: Vec<String> = tx
        .prepare("SELECT binding_id FROM bindings WHERE scope_id=?1 ORDER BY binding_id")?
        .query_map([scope.as_str()], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    keys.into_iter().map(id).collect()
}

pub(crate) fn page<T: Serialize>(
    total: u64,
    items: impl IntoIterator<Item = Result<T, RepositoryError>>,
) -> Result<Page<T>, RepositoryError> {
    let mut selected = Vec::new();
    let mut bytes = 0;
    for item in items {
        let item = item?;
        let size = encode(&item)?.len() + 1;
        if bytes + size > 3 * 1024 * 1024 {
            if selected.is_empty() {
                return Err(PapError::Persistence.into());
            }
            break;
        }
        bytes += size;
        selected.push(item);
    }
    Ok(Page {
        total,
        items: selected,
    })
}

pub(crate) fn validate_records(tx: &Transaction<'_>) -> Result<(), RepositoryError> {
    for (table, key) in [
        ("policies", "policy_id"),
        ("scopes", "scope_id"),
        ("bindings", "binding_id"),
    ] {
        let mut cursor = String::new();
        loop {
            let keys: Vec<String> = tx
                .prepare(&format!(
                    "SELECT {key} FROM {table} WHERE {key}>?1 ORDER BY {key} LIMIT 128"
                ))?
                .query_map([&cursor], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            if keys.is_empty() {
                break;
            }
            for key in &keys {
                let key = id(key.clone())?;
                match table {
                    "policies" => {
                        read_policy(tx, &key)?;
                    }
                    "scopes" => {
                        read_scope(tx, &key)?;
                    }
                    _ => {
                        read_binding(tx, &key)?;
                    }
                }
            }
            cursor = keys.last().cloned().ok_or(StoreError::Corrupt)?;
        }
    }
    let mut receipts=tx.prepare("SELECT last_write_id,last_write_digest,last_write_result_json FROM bindings WHERE last_write_id IS NOT NULL")?;
    let mut rows = receipts.query([])?;
    while let Some(row) = rows.next()? {
        uuid::Uuid::parse_str(&row.get::<_, String>(0)?).map_err(|_| StoreError::Corrupt)?;
        if row.get::<_, Vec<u8>>(1)?.len() != 32 {
            return Err(StoreError::Corrupt.into());
        }
        let receipt: WriteReceipt = decode(&row.get::<_, String>(2)?)?;
        if receipt.status_version.is_none_or(|v| v < 1) {
            return Err(StoreError::Corrupt.into());
        }
    }
    Ok(())
}
